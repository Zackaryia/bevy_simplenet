# Bevy Simplenet

Provides a bi-directional server/client channel implemented over websockets. This crate is suitable for user authentication, talking to a matchmaking service, communicating between micro-services, games that don't have strict latency requirements, etc.

- Client/server channel includes one-shot messages and a request/response API.
- Client message statuses can be tracked.
- Clients automatically work on native and WASM targets.
- Clients can be authenticated by the server (WIP).
- Provides optional server TLS.

Check out the example for a demonstration of how to build a Bevy client using this crate.

This crate requires nightly rust.



## Features

- `default`: includes `bevy`, `client`, `server` features
- `bevy`: derives `Resource` on [`Client`] and [`Server`]
- `client`: enables clients (native and WASM targets)
- `server`: enables servers (native-only targets)
- `tls-rustls`: enables TLS for servers via [`rustls`](https://crates.io/crates/rustls)
- `tls-openssl`: enables TLS for servers via [`OpenSSL`](https://crates.io/crates/openssl)



## WASM

On WASM targets the client backend will not update while any other tasks are running. You must either build an IO-oriented application that naturally spends a lot of time polling tasks, or manually release the main thread periodically (e.g. with `web_sys::Window::set_timeout_with_callback_and_timeout_and_arguments_0()`). For Bevy apps the latter happens automatically at the end of every app update/tick (see the `bevy::app::ScheduleRunnerPlugin` [implementation](https://github.com/bevyengine/bevy)).



## Usage notes

- Servers and clients must be created with [enfync](https://crates.io/crates/enfync) runtimes. The backend is [ezsockets](https://github.com/gbaranski/ezsockets).
- A client's [`AuthRequest`] type must match the corresponding server's [`Authenticator`] type.
- Client ids are defined by clients via their [`AuthRequest`] when connecting to a server. This means multiple sessions from the same client will have the same session id. Connections will be rejected if an id is already connected.
- Client connect messages will be cloned for all reconnect attempts, so they should be treated as static data.
- Server or client messages may fail to send if the underlying connection is broken. Clients can use the signals returned from [`Client::send()`] and [`Client::request()`] to track the status of a message. Client request results will always be emitted by [`Client::next()`]. Message tracking is not available for servers.
- Tracing levels assume the server is trusted and clients are not trusted.



## Example

```rust
// path shortcuts
use bevy_simplenet::{
    ChannelPack, ClientEventFrom, ServerEventFrom,
    ServerFactory, ClientFactory, ServerReport, ClientReport,
    AcceptorConfig, Authenticator, ServerConfig, AuthRequest,
    ClientConfig, MessageStatus, RequestStatus, EnvType
};
use serde::{Deserialize, Serialize};
use std::thread::sleep;
use std::time::Duration;


// define a channel
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TestConnectMsg(pub String);

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TestServerMsg(pub u64);

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TestClientMsg(pub u64);

#[derive(Debug, Clone)]
pub struct TestChannel;
impl ChannelPack for TestChannel
{
    type ConnectMsg = TestConnectMsg;
    type ServerMsg = TestServerMsg;
    type ServerResponse = ();
    type ClientMsg = TestClientMsg;
    type ClientRequest = ();
}

type TestClientEvent = ClientEventFrom<TestChannel>;
type TestServerEvent = ServerEventFrom<TestChannel>;

fn server_factory() -> ServerFactory<TestChannel>
{
    // It is recommended to make server/client factories with baked-in protocol versions (e.g.
    //   with env!("CARGO_PKG_VERSION")).
    ServerFactory::<TestChannel>::new("test")
}

fn client_factory() -> ClientFactory<TestChannel>
{
    // You must use the same protocol version string as the server factory.
    ClientFactory::<TestChannel>::new("test")
}


// enable tracing (with crate `tracing-subscriber`)
/*
let subscriber = tracing_subscriber::FmtSubscriber::builder()
    .with_max_level(tracing::Level::TRACE)
    .finish();
tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
tracing::info!("README test start");
*/


// make a server
let server = server_factory().new_server(
        enfync::builtin::native::TokioHandle::default(),
        "127.0.0.1:0",
        AcceptorConfig::Default,
        Authenticator::None,
        ServerConfig::default(),
    );
assert_eq!(server.num_connections(), 0u64);


// sleep duration for async machinery
let sleep_duration = Duration::from_millis(15);


// make a client
let client_id = 0u128;
let client = client_factory().new_client(
        enfync::builtin::Handle::default(),  //automatically selects native/WASM runtime
        server.url(),
        AuthRequest::None{ client_id },
        ClientConfig::default(),
        TestConnectMsg(String::from("hello"))
    );
sleep(sleep_duration);
assert_eq!(server.num_connections(), 1u64);


// read connection reports
let (
        client_id,
        TestServerEvent::Report(ServerReport::Connected(env_type, connect_msg))
    ) = server.next().unwrap() else { todo!(); };
let TestClientEvent::Report(ClientReport::Connected) = client.next().unwrap() else { todo!(); };
assert_eq!(env_type, EnvType::Native);
assert_eq!(connect_msg.0, String::from("hello"));


// send message: client -> server
let signal = client.send(TestClientMsg(42)).unwrap();
assert_eq!(signal.status(), MessageStatus::Sending);
sleep(sleep_duration);
assert_eq!(signal.status(), MessageStatus::Sent);


// read message from client
let (
        msg_client_id,
        TestServerEvent::Msg(TestClientMsg(msg_val))
    ) = server.next().unwrap() else { todo!() };
assert_eq!(msg_client_id, client_id);
assert_eq!(msg_val, 42);


// send message: server -> client
server.send(client_id, TestServerMsg(24)).unwrap();
sleep(sleep_duration);


// read message from server
let TestClientEvent::Msg(TestServerMsg(msg_server_val)) = client.next().unwrap() else { todo!() };
assert_eq!(msg_server_val, 24);


// send request to server
let signal = client.request(()).unwrap();
assert_eq!(signal.status(), RequestStatus::Sending);
sleep(sleep_duration);
assert_eq!(signal.status(), RequestStatus::Waiting);


// read request from client
let (_, TestServerEvent::Request((), request_token)) = server.next().unwrap() else { todo!() };


// acknowledge the request (consumes the token without sending a Response)
server.ack(request_token).unwrap();
sleep(sleep_duration);
assert_eq!(signal.status(), RequestStatus::Acknowledged);


// read acknowledgement from server
let TestClientEvent::Ack(request_id) = client.next().unwrap() else { todo!() };
assert_eq!(request_id, signal.id());


// client closes itself
client.close();
sleep(sleep_duration);
assert_eq!(server.num_connections(), 0u64);


// read disconnection messages
let (_, TestServerEvent::Report(ServerReport::Disconnected)) = server.next().unwrap() else { todo!() };
let TestClientEvent::Report(ClientReport::ClosedBySelf) = client.next().unwrap() else { todo!() };
let TestClientEvent::Report(ClientReport::IsDead(_)) = client.next().unwrap() else { todo!() };
```



## TODOs

- Fix linker errors when the `bevy/dynamic_linking` feature is enabled.
- Implement `AuthToken` for client/server authentication.
- Add server shut down procedure.
- Use const generics to bake protocol versions into `Server` and `Client` directly, instead of relying on factories (currently blocked by lack of robust compiler support).
- Move to stable rust once `HashMap::extract_if()` is stabilized.



## Bevy compatability

| bevy   | bevy_simplenet |
|--------|----------------|
| 0.11   | master         |
