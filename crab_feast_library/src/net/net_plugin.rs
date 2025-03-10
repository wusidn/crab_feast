

use std::{net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket}, time::SystemTime};

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use bevy_replicon_renet::{netcode::{ClientAuthentication, NetcodeClientTransport, NetcodeServerTransport, ServerAuthentication, ServerConfig}, renet::{ConnectionConfig, RenetClient, RenetServer}, RenetChannelsExt, RepliconRenetPlugins};

pub struct NetPlugin;

#[derive(States, Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum NetMode {
    #[default]
    None,
    Server,
    Client,
    // Hotseat,
}

#[derive(Resource)]
pub struct NetConfig {
    ip: IpAddr,
    port: u16, 
}

impl FromWorld for NetConfig {
    fn from_world(_world: &mut World) -> Self {
        NetConfig {
            ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 5000,
        }
    }
}

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app
        .add_plugins(RepliconPlugins)
        .add_plugins(RepliconRenetPlugins)
        .init_state::<NetMode>()
        .init_resource::<NetConfig>()
        .add_systems(OnEnter(NetMode::Server), Self::handle_server_mode)
        .add_systems(OnExit(NetMode::Server), Self::disable_server_mode)
        .add_systems(OnEnter(NetMode::Client), Self::handle_client_mode)
        .add_systems(OnExit(NetMode::Client), Self::disable_client_mode);
    }
}

impl NetPlugin {
    fn handle_server_mode(mut commands: Commands, 
        channels: Res<RepliconChannels>,
        config: Res<NetConfig>
    ) {
        let server_channels_config = channels.get_server_configs();
        let client_channels_config = channels.get_client_configs();
        let server = RenetServer::new(
            ConnectionConfig {
                server_channels_config,
                client_channels_config,
                ..Default::default()
            }
        );
        let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        let socket = UdpSocket::bind((config.ip, config.port)).unwrap();
        let server_config = ServerConfig {
            current_time,
            max_clients: 10,
            protocol_id: 0,
            authentication: ServerAuthentication::Unsecure,
            public_addresses: Default::default(),
        };
        let transport = NetcodeServerTransport::new(server_config, socket).unwrap();
        commands.insert_resource(server);
        commands.insert_resource(transport);
    }

    fn disable_server_mode(mut commands: Commands) {
        commands.remove_resource::<RenetServer>();
        commands.remove_resource::<NetcodeServerTransport>();
    }
    
    fn handle_client_mode(mut commands: Commands, 
        channels: Res<RepliconChannels>,
        config: Res<NetConfig>
    ) {
        let server_channels_config = channels.get_server_configs();
        let client_channels_config = channels.get_client_configs();

        let client = RenetClient::new(ConnectionConfig {
            server_channels_config,
            client_channels_config,
            ..Default::default()
        });

        let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        let client_id = current_time.as_millis() as u64;
        let server_addr = SocketAddr::new(config.ip, config.port);
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let authentication = ClientAuthentication::Unsecure {
            client_id,
            protocol_id: 0,
            server_addr,
            user_data: None,
        };
        let transport = NetcodeClientTransport::new(current_time, authentication, socket).unwrap();

        commands.insert_resource(client);
        commands.insert_resource(transport);
    }

    fn disable_client_mode(mut commands: Commands) {
        commands.remove_resource::<RenetClient>();
        commands.remove_resource::<NetcodeClientTransport>();
    }
    
    // fn handle_hotseat_mode(mut commands: Commands) {
        
    // }

    // fn disable_hotseat_mode(mut commands: Commands) {
    
    // }
}
