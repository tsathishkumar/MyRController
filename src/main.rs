#[macro_use]
extern crate diesel_migrations;
#[macro_use]
extern crate log;

use std::fs::create_dir_all;
use std::path::Path;
use std::thread;

use actix;
use actix::*;
use actix_web::{App, http::Method, middleware, middleware::cors::Cors, server};
use crossbeam_channel as channel;
use diesel::prelude::SqliteConnection;
use diesel::r2d2::{ConnectionManager, Pool};
use env_logger;
use ini::Ini;
use num_cpus;

use myscontroller_rs::api::{firmware, index, node, sensor};
use myscontroller_rs::api::index::AppState;
use myscontroller_rs::core::{connection, server as mys_controller};
use myscontroller_rs::core::connection::ConnectionType;
use myscontroller_rs::model::db;
use myscontroller_rs::wot;

fn main() {
    embed_migrations!("migrations");

    let sys = actix::System::new("webapp");

    let conf = match Ini::load_from_file("/etc/myscontroller-rs/conf.ini") {
        Ok(_conf) => _conf,
        Err(_) => Ini::load_from_file("conf.ini").unwrap(),
    };

    ::std::env::set_var("RUST_LOG", log_level(&conf));
    ::std::env::set_var("RUST_BACKTRACE", "1");
    env_logger::init();

    let database_url = server_configs(&conf);
    let manager = ConnectionManager::<SqliteConnection>::new(database_url);
    let conn = Pool::builder()
        .build(manager)
        .expect("Failed to create pool.");
    let conn_clone = conn.clone();
    let database_addr = SyncArbiter::start(num_cpus::get() * 4, move || db::ConnDsl(conn.clone()));

    let (controller_in_sender, controller_in_receiver) = channel::unbounded();
    let (out_set_sender, out_set_receiver) = channel::unbounded();
    let (in_set_sender, in_set_receiver) = channel::unbounded();
    let (new_sensor_sender, new_sensor_receiver) = channel::unbounded();
    let reset_signal_sender = controller_in_sender.clone();
    server::new(move || {
        App::with_state(AppState {
            db: database_addr.clone(),
            reset_sender: reset_signal_sender.clone(),
        })
            .middleware(middleware::Logger::default())
            .configure(|app| {
                Cors::for_app(app)
                    .allowed_methods(vec!["GET", "PUT", "POST", "DELETE"])
                    .resource("/", |r| {
                        r.method(Method::GET).h(index::home);
                    })
                    .resource("/nodes", |r| {
                        r.method(Method::GET).h(node::list);
                        r.method(Method::POST).with(node::create);
                        r.method(Method::PUT).with(node::update);
                        r.method(Method::DELETE).with(node::delete);
                    })
                    .resource("/nodes/{node_id}", |r| {
                        r.method(Method::GET).h(node::get_node);
                    })
                    .resource("/nodes/{node_id}/reboot", |r| {
                        r.method(Method::POST).h(node::reboot_node);
                    })
                    .resource("/sensors", |r| {
                        r.method(Method::GET).h(sensor::list);
                        r.method(Method::DELETE).with(node::delete);
                    })
                    .resource("/sensors/{node_id}/{child_sensor_id}", |r| {
                        r.method(Method::GET).h(sensor::get_sensor);
                    })
                    .resource("/firmwares", |r| {
                        r.method(Method::GET).h(firmware::list);
                    })
                    .resource("/firmwares/{firmware_type}/{firmware_version}", |r| {
                        r.method(Method::POST).with(firmware::create);
                        r.method(Method::PUT).with(firmware::update);
                        r.method(Method::DELETE).f(firmware::delete);
                    })
                    .resource("/firmwares/upload", |r| {
                        r.method(Method::GET).f(firmware::upload_form);
                    })
                    .register()
            })
    })
        .bind("0.0.0.0:8000")
        .unwrap()
        .shutdown_timeout(3)
        .start();

    match conn_clone.get() {
        Ok(conn) => embedded_migrations::run_with_output(&conn, &mut std::io::stdout()).unwrap(),
        Err(e) => error!("Error while running migration {:?}", e),
    };

    info!("Starting proxy server");

    let conn_pool_clone = conn_clone.clone();

    thread::spawn(move || {
        mys_controller::start(
            get_mys_gateway(&conf),
            get_mys_controller(&conf),
            conn_clone,
            controller_in_sender,
            controller_in_receiver,
            in_set_sender,
            out_set_receiver,
            new_sensor_sender,
        );
    });

    info!("Started proxy server");

    thread::spawn(move || {
        let (restart_sender, restart_receiver) = channel::unbounded();
        let restart_sender_clone = restart_sender.clone();
        let conn_pool = conn_pool_clone.clone();
        let out_set_sender_clone = out_set_sender.clone();
        let in_set_receiver_clone = in_set_receiver.clone();
        let new_sensor_receiver_clone = new_sensor_receiver.clone();
        wot::start_server(
            conn_pool,
            out_set_sender,
            in_set_receiver,
            new_sensor_receiver,
            restart_sender,
        );
        loop {
            let conn_pool_clone = conn_pool_clone.clone();
            let restart_sender_clone = restart_sender_clone.clone();
            let out_set_sender_clone = out_set_sender_clone.clone();
            let in_set_receiver_clone = in_set_receiver_clone.clone();
            let new_sensor_receiver_clone = new_sensor_receiver_clone.clone();
            match restart_receiver.recv() {
                Ok(_token) => wot::start_server(
                    conn_pool_clone,
                    out_set_sender_clone,
                    in_set_receiver_clone,
                    new_sensor_receiver_clone,
                    restart_sender_clone,
                ),
                Err(_e) => (),
            }
        }
    });

    info!("Started WoT server");

    sys.run();
}

pub fn server_configs(config: &Ini) -> String {
    let server_conf = config
        .section(Some("Server".to_owned()))
        .expect("Server configurations missing");
    let database_url = server_conf.get("database_url").expect(
        "database_url is not specified. Ex:database_url=/var/lib/myscontroller-rs/sqlite.db",
    );
    let database_path = Path::new(database_url);
    create_dir_all(database_path.parent().unwrap()).unwrap();
    database_url.to_owned()
}

pub fn log_level(config: &Ini) -> String {
    config
        .get_from(Some("Server"), "log_level")
        .unwrap_or("myscontroller_rs=info,actix_web=info")
        .to_owned()
}

fn get_mys_controller(config: &Ini) -> Option<connection::ConnectionType> {
    config.section(Some("Controller".to_owned())).map(|controller_conf| {
        let controller_type = controller_conf.get("type").expect("Controller port is not specified. Ex:\n\
     [Controller]\n type=SERIAL\n port=/dev/tty1\n or \n\n[Controller]\n type=SERIAL\n port=port=0.0.0.0:5003");
        let port = controller_conf.get("port").expect("Controller port is not specified. Ex:\n\
     [Controller]\n type=SERIAL\n port=/dev/tty1\n or \n\n[Controller]\n type=SERIAL\n port=port=0.0.0.0:5003");

        if controller_type == "Serial"
        {
            let baud_rate = controller_conf.get("baud_rate")
                .map(|baud_rate_str| baud_rate_str.parse::<u32>().unwrap_or(9600)).unwrap();
            return ConnectionType::Serial { port: port.to_owned(), baud_rate };
        }
        if controller_type == "TCP" {
            let timeout_enabled = controller_conf.get("timeout_enabled")
                .map(|baud_rate_str| baud_rate_str.parse::<bool>().unwrap())
                .unwrap_or(false);
            return ConnectionType::TcpServer { port: port.to_owned(), timeout_enabled };
        }
        let broker = controller_conf.get("broker").unwrap();
        let port_number = port.parse::<u16>().unwrap();
        let publish_topic_prefix = controller_conf.get("publish_topic_prefix").unwrap();
        return ConnectionType::MQTT { broker: broker.to_owned(), port: port_number, publish_topic_prefix: publish_topic_prefix.to_owned() };
    })
}

    fn get_mys_gateway(config: &Ini) -> connection::ConnectionType {
        config.section(Some("Gateway".to_owned())).map(|controller_conf| {
            let controller_type = controller_conf.get("type").unwrap();
            let port = controller_conf.get("port").unwrap();

            if controller_type == "Serial"
            {
                let baud_rate = controller_conf.get("baud_rate")
                    .map(|baud_rate_str| baud_rate_str.parse::<u32>().unwrap()).unwrap();
                return ConnectionType::Serial { port: port.to_owned(), baud_rate };
            }
            if controller_type == "TCP" {
                let timeout_enabled = controller_conf.get("timeout_enabled")
                    .map(|baud_rate_str| baud_rate_str.parse::<bool>().unwrap())
                    .unwrap_or(false);
                return ConnectionType::TcpClient { port: port.to_owned(), timeout_enabled };
            }
            let broker = controller_conf.get("broker").unwrap();
            let port_number = port.parse::<u16>().unwrap();
            let publish_topic_prefix = controller_conf.get("publish_topic_prefix").unwrap();
            return ConnectionType::MQTT { broker: broker.to_owned(), port: port_number, publish_topic_prefix: publish_topic_prefix.to_owned() };
        }).unwrap()
    }