//local shortcuts
use crate::*;

//third-party shortcuts
use bincode::Options;

//standard shortcuts
use core::fmt::Debug;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

//-------------------------------------------------------------------------------------------------------------------

/// A client for communicating with a [`Server`].
///
/// Use a [`ClientFactory`] to produce a new client.
///
/// It is safe to drop a client, however if you need a complete shut-down procedure then follow these steps:
/// 1) Call [`Client::close()`].
/// 2) Wait for [`Client::is_dead()`] to return true. Note that on WASM targets you cannot busy-wait since doing so
///    will block the client backend.
/// 3) Call [`Client::next()`] to drain any lingering events. [`ClientReport::IsDead`] will be the last event.
/// 4) Drop the client.
#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::system::Resource))]
pub struct Client<Channel: ChannelPack>
{
    /// this client's id
    client_id: u128,
    /// core websockets client
    client: ezsockets::Client<ClientHandler<Channel>>,
    /// sender for client events
    client_event_sender: crossbeam::channel::Sender<ClientEventFrom<Channel>>,
    /// receiver for client events
    client_event_receiver: crossbeam::channel::Receiver<ClientEventFrom<Channel>>,
    /// synchronized tracker for pending requests
    pending_requests: Arc<Mutex<PendingRequestTracker>>,
    /// signal for when the internal client is connected
    client_connected_signal: Arc<AtomicBool>,
    /// signal for when the internal client is shut down
    client_closed_signal: Arc<AtomicBool>,
    /// flag indicating the client closed itself
    closed_by_self: Arc<AtomicBool>,
}

impl<Channel: ChannelPack> Client<Channel>
{
    /// Send a one-shot message to the server.
    ///
    /// Returns `Ok(MessageSignal)` on success. The signal can be used to track the message status. Messages
    /// will fail if the underlying client becomes disconnected.
    ///
    /// Returns `Err` if the client is not connected.
    pub fn send(&self, msg: Channel::ClientMsg) -> Result<MessageSignal, ()>
    {
        // check if connected
        if !self.is_connected() { tracing::warn!("tried to send message to disconnected client"); return Err(()); }

        // forward message to server
        let Ok(ser_msg) = bincode::DefaultOptions::new().serialize(&ServerMetaEventFrom::<Channel>::Msg(msg))
        else { tracing::error!("failed serializing client message"); return Err(()); };

        match self.client.binary(ser_msg)
        {
            Ok(signal) => Ok(signal),
            Err(_) =>
            {
                tracing::warn!("tried to send message to dead client");
                Err(())
            }
        }
    }

    /// Send a request to the server.
    ///
    /// Returns `Ok(RequestSignal)` on success. The signal can be used to track the message status. Requests
    /// will fail if the underlying client becomes disconnected.
    ///
    /// Returns `Err` if the client is not connected.
    pub fn request(&self, request: Channel::ClientRequest) -> Result<RequestSignal, ()>
    {
        // lock pending requests
        let Ok(mut pending_requests) = self.pending_requests.lock() else { return Err(()); };

        // check if connected
        // - We do this after locking the pending requests cache in order to synchronize with dropping the internal
        //   client handler, and to synchronize with reconnect cycles in the client backend.
        if !self.is_connected() { tracing::warn!("tried to send request to disconnected client"); return Err(()); };

        // prep request id
        let request_id = pending_requests.reserve_id();

        // forward message to server
        let Ok(ser_msg) = bincode::DefaultOptions::new().serialize(
                &ServerMetaEventFrom::<Channel>::Request(request, request_id)
            )
        else { tracing::error!("failed serializing client request"); return Err(()); };

        match self.client.binary(ser_msg)
        {
            Ok(signal) =>
            {
                let request_signal = pending_requests.add_request(request_id, signal);
                Ok(request_signal)
            }
            Err(_) =>
            {
                tracing::warn!("tried to send request to dead client");
                Err(())
            }
        }
    }

    /// Try to get the next client event.
    ///
    /// When the client dies, the last event emitted will be `ClientEvent::Report(ClientReport::IsDead))`.
    pub fn next(&self) -> Option<ClientEventFrom<Channel>>
    {
        let Ok(msg) = self.client_event_receiver.try_recv() else { return None; };
        Some(msg)
    }

    /// Access this client's id.
    pub fn id(&self) -> u128
    {
        self.client_id
    }

    /// Test if the client is connected.
    ///
    /// Messages and requests cannot be submitted when the client is not connected.
    pub fn is_connected(&self) -> bool
    {
        self.client_connected_signal.load(Ordering::Acquire) && !self.is_closed()
    }

    /// Test if the client is dead (no longer connected to the server and won't reconnect).
    /// - Note that [`ClientReport::IsDead`] will be emitted by [`Client::next()`] when the client backend dies.
    ///
    /// Once this returns true you can drain the client by calling [`Client::next()`] until no more values appear.
    /// After [`ClientReport::IsDead`] appears, [`Client::next()`] will always return `None`.
    pub fn is_dead(&self) -> bool
    {
        self.client_closed_signal.load(Ordering::Acquire)
    }

    /// Test if the client is closed.
    ///
    /// Returns true after [`Client::close()`] has been called, or once the internal client dies.
    ///
    /// Messages and requests cannot be submitted once the client is closed.
    pub fn is_closed(&self) -> bool
    {
        self.closed_by_self.load(Ordering::Acquire) || self.is_dead()
    }

    /// Close the client.
    ///
    /// Any in-progress messages may or may not fail once this method is called. New messages and requests cannot be
    /// sent after this method is called.
    ///
    /// The client will eventually emit [`ClientReport::IsDead`] once this method has been called.
    pub fn close(&self)
    {
        // sanity check
        if self.is_closed() { tracing::warn!("tried to close an already closed client"); return; }
        tracing::info!("client closing self");

        // close the client
        let closure_frame =
            ezsockets::CloseFrame{
                code   : ezsockets::CloseCode::Normal,
                reason : String::from("client done")
            };
        if self.client.close(Some(closure_frame)).is_err()
        {
            tracing::warn!("tried to close an already dead client");
            return;
        }

        // forward event to other end of channel
        if let Err(err) = self.client_event_sender.send(ClientEventFrom::<Channel>::Report(ClientReport::ClosedBySelf))
        {
            tracing::error!(?err, "failed to forward connection event to client");
        }

        // note: request failures will be emitted for all pending requests when the internal client is dropped

        // mark the client as closed
        self.closed_by_self.store(true, Ordering::Release);
    }
}

impl<Channel: ChannelPack> Drop for Client<Channel>
{
    fn drop(&mut self)
    {
        if self.is_dead() { return; }
        self.close();
    }
}

//-------------------------------------------------------------------------------------------------------------------

/// Factory for producing [`Client`]s that all bake in the same protocol version.
//todo: use const generics on the protocol version instead (currently broken, async methods cause compiler errors)
#[derive(Debug, Clone)]
pub struct ClientFactory<Channel: ChannelPack>
{
    protocol_version : &'static str,
    _phantom         : PhantomData<Channel>,
}

impl<Channel: ChannelPack> ClientFactory<Channel>
{
    /// Make a new server factory with a given protocol version.
    pub fn new(protocol_version: &'static str) -> Self
    {
        ClientFactory{ protocol_version, _phantom: PhantomData::default() }
    }

    /// New client.
    pub fn new_client(&self,
        runtime_handle : enfync::builtin::Handle,
        url            : url::Url,
        auth           : AuthRequest,
        config         : ClientConfig,
        connect_msg    : Channel::ConnectMsg,
    ) -> Client<Channel>
    {
        // prepare to make client connection
        // note: urls cannot contain raw bytes so we must serialize as json
        let auth_msg_ser    = serde_json::to_string(&auth).expect("could not serialize authentication");
        let connect_msg_ser = serde_json::to_string(&connect_msg).expect("could not serialize connect msg");

        let client_config = ezsockets::ClientConfig::new(url)
            .reconnect_interval(config.reconnect_interval)
            .max_initial_connect_attempts(config.max_initial_connect_attempts)
            .max_reconnect_attempts(config.max_reconnect_attempts)
            .query_parameter(VERSION_MSG_KEY, self.protocol_version)
            .query_parameter(TYPE_MSG_KEY, env_type_as_str(env_type()))
            .query_parameter(AUTH_MSG_KEY, auth_msg_ser.as_str())
            .query_parameter(CONNECT_MSG_KEY, connect_msg_ser.as_str());

        // prepare client's socket config
        let mut socket_config = ezsockets::SocketConfig::default();
        socket_config.heartbeat = config.heartbeat_interval;
        socket_config.timeout   = config.keepalive_timeout;

        #[cfg(target_family = "wasm")]
        {
            // on WASM we need custom Ping/Pong protocol
            socket_config.heartbeat_ping_msg_fn = Arc::new(text_ping_fn);
        }

        let client_config = client_config.socket_config(socket_config);

        // prepare message channel that points out of our client
        let (client_event_sender, client_event_receiver) = crossbeam::channel::unbounded::<ClientEventFrom<Channel>>();

        // prepare client connector
        let client_connector = {
                #[cfg(not(target_family = "wasm"))]
                { ezsockets::ClientConnectorTokio::from(runtime_handle.clone()) }

                #[cfg(target_family = "wasm")]
                { ezsockets::ClientConnectorWasm::default() }
            };

        // make client core with our handler
        let client_event_sender_clone = client_event_sender.clone();
        let pending_requests = Arc::new(Mutex::new(PendingRequestTracker::default()));
        let pending_requests_clone = pending_requests.clone();
        let client_connected_signal = Arc::new(AtomicBool::new(false));
        let client_closed_signal = Arc::new(AtomicBool::new(false));
        let client_connected_signal_clone = client_connected_signal.clone();
        let client_closed_signal_clone = client_closed_signal.clone();
        let (client, _client_task_handle) = ezsockets::connect_with(
                move |client|
                {
                    ClientHandler::<Channel>{
                            config,
                            client,
                            client_event_sender     : client_event_sender_clone,
                            pending_requests        : pending_requests_clone,
                            client_connected_signal : client_connected_signal_clone,
                            client_closed_signal    : client_closed_signal_clone,
                        }
                },
                client_config,
                client_connector,
            );

        // finish assembling our client
        tracing::info!("created new client");

        Client{
                client_id: auth.client_id(),
                client,
                client_event_sender,
                client_event_receiver,
                pending_requests,
                client_connected_signal,
                client_closed_signal,
                closed_by_self: Arc::new(AtomicBool::new(false)),
            }
    }
}

//-------------------------------------------------------------------------------------------------------------------
