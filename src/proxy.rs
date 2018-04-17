use diesel::prelude::*;
use gateway::*;
use interceptor;
use node;
use ota;
use std::sync::mpsc;
use std::thread;

pub fn start(mut mys_gateway_writer: Box<Gateway>,
             mut mys_controller_writer: Box<Gateway>, db_connection: SqliteConnection) -> Result<String, String> {
    let mut mys_gateway_reader = mys_gateway_writer.clone();
    let mut mys_controller_reader = mys_controller_writer.clone();

    let (gateway_sender, gateway_receiver) = mpsc::channel();
    let (ota_sender, ota_receiver) = mpsc::channel();
    let (controller_in_sender, controller_in_receiver) = mpsc::channel();
    let (controller_out_sender, controller_out_receiver) = mpsc::channel();
    let (node_manager_sender, node_manager_in) = mpsc::channel();
    let ota_fw_sender = controller_in_sender.clone();
    let node_manager_out = controller_in_sender.clone();

    let gateway_reader = thread::spawn(move || {
        mys_gateway_reader.read_loop(&gateway_sender);
    });
    let controller_reader = thread::spawn(move || {
        mys_controller_reader.read_loop(&controller_in_sender);
    });

    let message_interceptor = thread::spawn(move || {
        interceptor::intercept(&gateway_receiver, &ota_sender, &node_manager_sender, &controller_out_sender);
    });

    let gateway_writer = thread::spawn(move || {
        mys_gateway_writer.write_loop(&controller_in_receiver);
    });

    let controller_writer = thread::spawn(move || {
        mys_controller_writer.write_loop(&controller_out_receiver);
    });

    let ota_processor = thread::spawn(move || {
        ota::process_ota(&ota_receiver, &ota_fw_sender);
    });

    let node_manager = thread::spawn(move || {
        node::handle_node_id_request(&node_manager_in, &node_manager_out, db_connection);
    });

    match message_interceptor.join() {
        Ok(_result) => (),
        Err(_error) => return Err(String::from("Error in Message interceptor")),
    }
    match gateway_reader.join() {
        Ok(_result) => (),
        Err(_error) => return Err(String::from("Error in Gateway reader")),
    };
    match controller_reader.join() {
        Ok(_) => (),
        _ => return Err(String::from("Error in Controller reader")),
    }
    match gateway_writer.join() {
        Ok(_) => (),
        _ => return Err(String::from("Error in Gateway writer")),
    };
    match controller_writer.join() {
        Ok(_) => (),
        _ => return Err(String::from("Error in Controller writer")),
    };
    match ota_processor.join() {
        Ok(_) => (),
        _ => return Err(String::from("Error in OTA processor")),
    };
    match node_manager.join() {
        Ok(_) => (),
        _ => return Err(String::from("Error in Node Manager")),
    };

    Ok(String::from("Done"))
}
