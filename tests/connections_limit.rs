//local shortcuts

//third-party shortcuts
use serde::{Serialize, Deserialize};

//standard shortcuts
use std::vec::Vec;

//-------------------------------------------------------------------------------------------------------------------
//-------------------------------------------------------------------------------------------------------------------

/// message from server
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DemoServerMsg(pub u64);

/// message from client
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DemoClientMsg(pub u64);

/// client connect message
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DemoConnectMsg(pub String);

type ServerDemo = bevy_simplenet::Server::<DemoServerMsg, DemoClientMsg, DemoConnectMsg>;
type ClientDemo = bevy_simplenet::Client::<DemoServerMsg, DemoClientMsg, DemoConnectMsg>;

fn server_demo_factory() -> ServerDemo::Factory
{
    ServerDemo::Factory::new("test")
}

fn client_demo_factory() -> ClientDemo::Factory
{
    ClientDemo::Factory::new("test")
}

//-------------------------------------------------------------------------------------------------------------------
//-------------------------------------------------------------------------------------------------------------------

fn connections_limit_test(max_connections: u32)
{
    assert!(max_connections > 0);

    // prepare tokio runtimes for server and client
    let server_runtime = bevy_simplenet::DefaultIORuntime::default();
    let client_runtime = bevy_simplenet::DefaultIORuntime::default();

    // prepare connection acceptor
    let plain_acceptor = ezsockets::tungstenite::Acceptor::Plain;

    // launch websocket server
    let websocket_server = server_demo_factory().new_server(
            server_runtime,
            "127.0.0.1:0",
            plain_acceptor,
            bevy_simplenet::Authenticator::None,
            bevy_simplenet::ServerConnectionConfig{
                max_connections,
                max_msg_size: 10_000,
                rate_limit_config : bevy_simplenet::RateLimitConfig{
                        period    : std::time::Duration::from_millis(15),
                        max_count : 25
                    }
            }
        );

    let websocket_url = websocket_server.url();


    // 1. connect 'max connections' clients
    let mut clients = Vec::new();
    let connect_msg = DemoConnectMsg(String::from("hello"));

    for client_num in 0..max_connections
    {
        // make client (block until connected)
        let bevy_simplenet::PRResult::Result(websocket_client) = client_demo_factory().new_client(
                client_runtime.clone(),
                websocket_url.clone(),
                bevy_simplenet::AuthRequest::None{ client_id: client_num as u128 },
                bevy_simplenet::ClientConnectionConfig::default(),
                connect_msg.clone()
            ).extract() else { panic!(""); };

        std::thread::sleep(std::time::Duration::from_millis(25));  //wait for async machinery

        // client should connect
        assert!(!websocket_client.is_dead());
        let Some(bevy_simplenet::ClientConnectionReport::Connected) = websocket_client.try_get_next_connection_report()
        else { panic!("client should be connected to server"); };
        let Some(bevy_simplenet::ServerConnectionReport::Connected(_, _)) = websocket_server.try_get_next_connection_report()
        else { panic!("server should be connected to client: {}", client_num); };

        clients.push(websocket_client);
    }

    // 2. connecting one more client should fail
    // make client (block until connected)
    let bevy_simplenet::PRResult::Result(websocket_client) = client_demo_factory().new_client(
            client_runtime.clone(),
            websocket_url.clone(),
            bevy_simplenet::AuthRequest::None{ client_id: 92748u128 },
            bevy_simplenet::ClientConnectionConfig::default(),
            connect_msg.clone()
        ).extract() else { panic!(""); };

    std::thread::sleep(std::time::Duration::from_millis(25));  //wait for async machinery

    // client should get closed by the server immediately
    assert!(websocket_client.is_dead());
    let Some(bevy_simplenet::ClientConnectionReport::Connected) = websocket_client.try_get_next_connection_report()
    else { panic!("client should be connected to server"); };
    let Some(bevy_simplenet::ClientConnectionReport::ClosedByServer(_)) = websocket_client.try_get_next_connection_report()
    else { panic!("client should be closed by server"); };
    let None = websocket_server.try_get_next_connection_report()
    else { panic!("server should not connect to another client"); };

    // 3. disconnect one client
    let client_to_disconnect = clients.pop().expect("there should be at least one connected client");
    client_to_disconnect.close();

    std::thread::sleep(std::time::Duration::from_millis(25));  //wait for async machinery

    let Some(bevy_simplenet::ClientConnectionReport::ClosedBySelf) = client_to_disconnect.try_get_next_connection_report()
    else { panic!("client should be closed by self"); };
    let Some(bevy_simplenet::ServerConnectionReport::Disconnected(_)) = websocket_server.try_get_next_connection_report()
    else { panic!("server should see a disconnected client"); };

    // 4. adding a client should now succeed
    // make client (block until connected)
    let bevy_simplenet::PRResult::Result(websocket_client) = client_demo_factory().new_client(
            client_runtime.clone(),
            websocket_url.clone(),
            bevy_simplenet::AuthRequest::None{ client_id: 64819u128 },
            bevy_simplenet::ClientConnectionConfig::default(),
            connect_msg.clone()
        ).extract() else { panic!(""); };

    std::thread::sleep(std::time::Duration::from_millis(25));  //wait for async machinery

    // client should connect
    assert!(!websocket_client.is_dead());
    let Some(bevy_simplenet::ClientConnectionReport::Connected) = websocket_client.try_get_next_connection_report()
    else { panic!("client should be connected to server"); };
    let Some(bevy_simplenet::ServerConnectionReport::Connected(_, _)) = websocket_server.try_get_next_connection_report()
    else { panic!("server should be connected to client"); };

    clients.push(websocket_client);  //save client so it doesn't get dropped

    // 5. connecting one more client should fail
    // make client (block until connected)
    let bevy_simplenet::PRResult::Result(websocket_client) = client_demo_factory().new_client(
            client_runtime.clone(),
            websocket_url.clone(),
            bevy_simplenet::AuthRequest::None{ client_id: 15364898u128 },
            bevy_simplenet::ClientConnectionConfig::default(),
            connect_msg.clone()
        ).extract() else { panic!(""); };

    std::thread::sleep(std::time::Duration::from_millis(25));  //wait for async machinery

    // client should not connect
    assert!(websocket_client.is_dead());
    let Some(bevy_simplenet::ClientConnectionReport::Connected) = websocket_client.try_get_next_connection_report()
    else { panic!("client should be connected to server"); };
    let Some(bevy_simplenet::ClientConnectionReport::ClosedByServer(_)) = websocket_client.try_get_next_connection_report()
    else { panic!("client should be closed by server"); };
    let None = websocket_server.try_get_next_connection_report()
    else { panic!("server should not connect to another client"); };


    // no more connection reports
    let None = websocket_server.try_get_next_connection_report()
    else { panic!("server should receive no more connection reports"); };
    let None = websocket_client.try_get_next_connection_report()
    else { panic!("client should receive no more connection reports"); };
}

//-------------------------------------------------------------------------------------------------------------------
//-------------------------------------------------------------------------------------------------------------------

#[test]
fn bevy_simplenet_connections_limit()
{
    connections_limit_test(1);
    connections_limit_test(2);
    connections_limit_test(10);
}

//-------------------------------------------------------------------------------------------------------------------
