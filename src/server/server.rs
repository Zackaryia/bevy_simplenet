//local shortcuts
use crate::*;

//third-party shortcuts
use axum::response::IntoResponse;
use enfync::Handle;

//standard shortcuts
use core::fmt::Debug;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::marker::PhantomData;
use std::sync::Arc;

//-------------------------------------------------------------------------------------------------------------------
//-------------------------------------------------------------------------------------------------------------------

fn socket_config(prevalidator: &ConnectionPrevalidator, client_env_type: EnvType) -> ezsockets::SocketConfig
{
    match client_env_type
    {
        EnvType::Native =>
        {
            // use the default heartbeat ping message function
            ezsockets::SocketConfig{
                heartbeat : prevalidator.heartbeat_interval,
                timeout   : prevalidator.keepalive_timeout,
                ..Default::default()
            }
        }
        EnvType::Wasm =>
        {
            // use a custom Text-based ping message
            ezsockets::SocketConfig{
                    heartbeat : prevalidator.heartbeat_interval,
                    timeout   : prevalidator.keepalive_timeout,
                    heartbeat_ping_msg_fn : Arc::new(text_ping_fn)
                }
        }
    }
}

//-------------------------------------------------------------------------------------------------------------------
//-------------------------------------------------------------------------------------------------------------------

async fn websocket_handler<Channel: ChannelPack>(
    axum::Extension(server) : axum::Extension<ezsockets::Server<ConnectionHandler<Channel>>>,
    axum::Extension(count)  : axum::Extension<ConnectionCounter>,
    axum::Extension(preval) : axum::Extension<Arc<ConnectionPrevalidator>>,
    ezsocket_upgrade        : ezsockets::axum::Upgrade,
) -> impl axum::response::IntoResponse
{
    // prevalidate then prepare upgrade
    match prevalidate_connection_request(ezsocket_upgrade.request(), &count, &preval)
    {
        Ok(client_env_type) => ezsocket_upgrade.on_upgrade_with_config(server, socket_config(&preval, client_env_type)),
        Err(err) => err.into_response()
    }
}

//-------------------------------------------------------------------------------------------------------------------
//-------------------------------------------------------------------------------------------------------------------

async fn run_server(app: axum::Router, listener: std::net::TcpListener, acceptor_config: AcceptorConfig)
{
    // set listener
    let server = axum_server::Server::from_tcp(listener);

    // set acceptor
    let server = match acceptor_config
    {
        AcceptorConfig::Default         => server.acceptor(axum_server::accept::DefaultAcceptor::new()),
        #[cfg(feature = "tls-rustls")]
        AcceptorConfig::Rustls(config)  => server.acceptor(axum_server::tls_rustls::RustlsAcceptor::new(config)),
        #[cfg(feature = "tls-openssl")]
        AcceptorConfig::OpenSSL(config) => server.acceptor(axum_server::tls_openssl::OpenSSLAcceptor::new(config)),
    };

    // serve it
    if let Err(err) = server.serve(app.into_make_service_with_connect_info::<SocketAddr>()).await
    {
        tracing::error!(?err, "server stopped running with error");
    }
}

//-------------------------------------------------------------------------------------------------------------------
//-------------------------------------------------------------------------------------------------------------------

/// A server for communicating with [`Client`]s.
///
/// Use a [`ServerFactory`] to produce a new server.
///
/// Note that the server does not currently have a shut-down procedure other than closing the executable.
#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::system::Resource))]
pub struct Server<Channel: ChannelPack>
{
    /// The server's address.
    server_address: SocketAddr,
    /// Indicates whether or not the server uses TLS.
    uses_tls: bool,
    /// The number of current connections.
    connection_counter: ConnectionCounter,

    /// Sends client events to the internal connection handler.
    client_event_sender: tokio::sync::mpsc::UnboundedSender<
        SessionTargetMsg<SessionID, SessionCommand<Channel>>
    >,
    /// Receives server events from the internal connection handler.
    server_event_receiver: crossbeam::channel::Receiver<SessionSourceMsg<SessionID, ServerEventFrom<Channel>>>,

    /// A signal that indicates if the server's internal worker has stopped.
    server_closed_signal: enfync::PendingResult<()>,
    /// A signal that indicates if the server runner has stopped.
    server_running_signal: enfync::PendingResult<()>,
}

impl<Channel: ChannelPack> Server<Channel>
{
    /// Send a message to the target session.
    /// - Messages will be silently dropped if the session is not connected (there may or may not be a trace message).
    /// - Returns `Err` if an internal server error occurs.
    pub fn send(&self, id: SessionID, msg: Channel::ServerMsg) -> Result<(), ()>
    {
        if self.is_dead() { tracing::warn!(id, "tried to send message to session but server is dead"); return Err(()); }

        // send to endpoint of ezsockets::Server::call() (will be picked up by ConnectionHandler::on_call())
        if let Err(err) = self.client_event_sender.send(
                SessionTargetMsg::new(id, SessionCommand::<Channel>::Send(ClientMetaEventFrom::<Channel>::Msg(msg), None))
            )
        {
            tracing::error!(?err, "failed to forward message to session");
            return Err(());
        }

        Ok(())
    }

    /// Respond to a client request.
    /// - Messages will be silently dropped if the session is not connected (there may or may not be a trace message).
    /// - Returns `Err` if an internal server error occurs.
    pub fn respond(&self, token: RequestToken, response: Channel::ServerResponse) -> Result<(), ()>
    {
        // check server liveness
        let client_id  = token.client_id();
        let request_id = token.request_id();
        if self.is_dead()
        {
            tracing::warn!(client_id, request_id, "tried to send response to session but server is dead");
            return Err(());
        }

        // check token liveness
        if token.destination_is_dead()
        {
            tracing::debug!(client_id, request_id, "tried to send response to dead session");
            return Ok(());
        }

        // send to endpoint of ezsockets::Server::call() (will be picked up by ConnectionHandler::on_call())
        let (request_id, death_signal) = token.take();
        if let Err(err) = self.client_event_sender.send(SessionTargetMsg::new(
                client_id,
                SessionCommand::<Channel>::Send(
                    ClientMetaEventFrom::<Channel>::Response(response, request_id),
                    Some(death_signal)
                )
            ))
        {
            tracing::error!(?err, "failed to forward response to session");
            return Err(());
        }

        Ok(())
    }

    /// Acknowledge a client request.
    /// - Messages will be silently dropped if the session is not connected (there may or may not be a trace message).
    /// - Returns `Err` if an internal server error occurs.
    ///
    /// An acknowledged request cannot be responded to.
    pub fn ack(&self, token: RequestToken) -> Result<(), ()>
    {
        // check server liveness
        let client_id  = token.client_id();
        let request_id = token.request_id();
        if self.is_dead()
        {
            tracing::warn!(client_id, request_id, "tried to send ack to session but server is dead");
            return Err(());
        }

        // check token liveness
        if token.destination_is_dead()
        {
            tracing::debug!(client_id, request_id, "tried to send response to dead session");
            return Ok(());
        }

        // send to endpoint of ezsockets::Server::call() (will be picked up by ConnectionHandler::on_call())
        let (request_id, death_signal) = token.take();
        if let Err(err) = self.client_event_sender.send(SessionTargetMsg::new(
                client_id,
                SessionCommand::<Channel>::Send(ClientMetaEventFrom::<Channel>::Ack(request_id), Some(death_signal))
            ))
        {
            tracing::error!(?err, "failed to forward ack to session");
            return Err(());
        }

        Ok(())
    }

    /// Reject a client request.
    pub fn reject(&self, _token: RequestToken)
    {
        // drop the token: rejection will happen automatically using the token's custom Drop
    }

    /// Close the target session.
    ///
    /// The target session may remain open until some time after this method is called.
    pub fn close_session(&self, id: SessionID, close_frame: ezsockets::CloseFrame) -> Result<(), ()>
    {
        // send to endpoint of ezsockets::Server::call() (will be picked up by ConnectionHandler::on_call())
        tracing::info!(id, "closing client");
        if self.is_dead()
        {
            tracing::warn!(id, "tried to close session but server is dead");
            return Err(());
        }
        if let Err(err) = self.client_event_sender.send(
                SessionTargetMsg::new(id, SessionCommand::<Channel>::Close(close_frame))
            )
        {
            tracing::error!(?err, "failed to forward session close command to session");
            return Err(());
        }

        Ok(())
    }

    /// Get the next available server event
    pub fn next(&self) -> Option<(SessionID, ServerEventFrom<Channel>)>
    {
        let Ok(msg) = self.server_event_receiver.try_recv() else { return None; };
        Some((msg.id, msg.msg))
    }

    /// Get the server's url.
    pub fn url(&self) -> url::Url
    {
        make_websocket_url(self.uses_tls, self.server_address).unwrap()
    }

    /// Get the number of client connections.
    pub fn num_connections(&self) -> u64
    {
        self.connection_counter.load()
    }

    /// Test if the server is dead.
    pub fn is_dead(&self) -> bool
    {
        self.server_closed_signal.done() || self.server_running_signal.done()
    }
}

//-------------------------------------------------------------------------------------------------------------------

/// Factory for producing servers that all bake in the same protocol version.
//todo: use const generics on the protocol version instead (currently broken, async methods cause compiler errors)
#[derive(Debug, Clone)]
pub struct ServerFactory<Channel: ChannelPack>
{
    protocol_version : &'static str,
    _phantom         : PhantomData<Channel>,
}

impl<Channel: ChannelPack> ServerFactory<Channel>
{
    /// Make a new server factory with a given protocol version.
    pub fn new(protocol_version: &'static str) -> Self
    {
        ServerFactory{ protocol_version, _phantom: PhantomData::default() }
    }

    /// Make a new server.
    ///
    /// Only works with a tokio runtime handle.
    pub fn new_server<A>(&self,
        runtime_handle  : enfync::builtin::native::TokioHandle,
        address         : A,
        acceptor_config : AcceptorConfig,
        authenticator   : Authenticator,
        config          : ServerConfig
    ) -> Server<Channel>
    where
        A: std::net::ToSocketAddrs + Send + 'static,
    {
        // prepare message channel that points out of the connection handler
        let (
                server_event_sender,
                server_event_receiver
            ) = crossbeam::channel::unbounded::<SessionSourceMsg<SessionID, ServerEventFrom<Channel>>>();

        // prepare connection counter
        // - this is used to communication the current number of connections from the connection handler to the
        //   connection prevalidator
        let connection_counter = ConnectionCounter::default();

        // make server core with our connection handler
        // note: ezsockets::Server::create() must be called from within a tokio runtime
        let connection_counter_clone = connection_counter.clone();

        let (server, server_worker) = enfync::blocking::extract(runtime_handle.spawn(async move {
                ezsockets::Server::create(
                        move |_server|
                        ConnectionHandler::<Channel>{
                                config,
                                connection_counter: connection_counter_clone,
                                session_registry: HashMap::default(),
                                server_event_sender,
                            }
                    )
            })).unwrap();

        let server_closed_signal = runtime_handle.spawn(
                async move {
                    if let Err(err) = server_worker.await
                    {
                        tracing::error!(?err, "server closed with error");
                    }
                }
            );

        // prepare prevalidator
        let prevalidator = ConnectionPrevalidator{
                protocol_version   : self.protocol_version,
                authenticator,
                max_connections    : config.max_connections,
                max_msg_size       : config.max_msg_size,
                heartbeat_interval : config.heartbeat_interval,
                keepalive_timeout  : config.keepalive_timeout,
            };

        // prepare router
        let app = axum::Router::new()
            .route("/ws", axum::routing::get(websocket_handler::<Channel>))
            .layer(axum::Extension(server.clone()))
            .layer(axum::Extension(Arc::new(prevalidator)))
            .layer(axum::Extension(connection_counter.clone()));

        // prepare listener
        let connection_listener = std::net::TcpListener::bind(address).unwrap();
        let server_address = connection_listener.local_addr().unwrap();
        let uses_tls = !matches!(acceptor_config, AcceptorConfig::Default);

        // launch the server core
        let server_running_signal = runtime_handle.spawn(
                async move { run_server(app, connection_listener, acceptor_config).await }
            );

        // finish assembling our server
        tracing::info!("new server created");
        Server{
                server_address,
                uses_tls,
                connection_counter,
                client_event_sender: server.into(),  //extract the call sender
                server_event_receiver,
                server_closed_signal,
                server_running_signal,
            }
    }
}

//-------------------------------------------------------------------------------------------------------------------
