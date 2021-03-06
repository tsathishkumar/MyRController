use diesel;
use diesel::prelude::*;
use diesel::r2d2::ConnectionManager;
use r2d2::*;

use crate::channel::{Receiver, Sender};
use crate::core::message::presentation::PresentationMessage;
use crate::model::node::Node;
use crate::model::node::nodes;
use crate::model::sensor::Sensor;
use crate::model::sensor::sensors::dsl::*;

pub fn handle(
    receiver: &Receiver<PresentationMessage>,
    sender: &Sender<String>,
    db_connection: PooledConnection<ConnectionManager<SqliteConnection>>,
    new_sensor_sender: Sender<(String, Sensor)>,
) {
    loop {
        if let Ok(presentation_message) = receiver.recv() {
            create_or_update_sensor(&db_connection, &presentation_message, &new_sensor_sender);
            match sender.send(presentation_message.to_string()) {
                Ok(_) => (),
                Err(_) => error!("Error while forwarding presentation message"),
            }
        }
    }
}


pub fn create_or_update_sensor(
    conn: &SqliteConnection,
    presentation_message: &PresentationMessage,
    new_sensor_sender: &Sender<(String, Sensor)>,
) {
    let sensor_message = Sensor {
        node_id: i32::from(presentation_message.node_id),
        child_sensor_id: i32::from(presentation_message.child_sensor_id),
        sensor_type: presentation_message.sub_type,
        description: presentation_message.payload.clone(),
    };

    match nodes::dsl::nodes
        .find(sensor_message.node_id)
        .first::<Node>(conn)
        {
            Ok(node) => create_or_update_child_sensor(&conn, node, sensor_message, new_sensor_sender),
            Err(diesel::result::Error::NotFound) => {
                info!(
                    "Node doesn't exist for {:?}, Creating new node",
                    &sensor_message
                );
                match super::internal::create_node(&conn, sensor_message.node_id) {
                    Ok(node) => {
                        create_or_update_child_sensor(&conn, node, sensor_message, new_sensor_sender)
                    }
                    Err(e) => error!(
                        "Error while creating new node for {}, {:?}",
                        sensor_message.node_id, e
                    ),
                }
            }
            Err(_e) => error!(
                "Error while checking for existing node for {}, {:?}",
                sensor_message.node_id, _e
            ),
        }
}

pub fn create_or_update_child_sensor(
    conn: &SqliteConnection,
    node: Node,
    sensor_message: Sensor,
    new_sensor_sender: &Sender<(String, Sensor)>,
) {
    match sensors
        .find((sensor_message.node_id, sensor_message.child_sensor_id))
        .first::<Sensor>(conn)
        {
            Ok(existing_sensor) => {
                if existing_sensor != sensor_message {
                    match diesel::update(sensors)
                        .filter(node_id.eq(sensor_message.node_id))
                        .filter(child_sensor_id.eq(sensor_message.child_sensor_id))
                        .set((
                            sensor_type.eq(sensor_message.sensor_type),
                            description.eq(&sensor_message.description),
                        ))
                        .execute(conn)
                        {
                            Ok(_) => info!("Updated sensor {:?}", &sensor_message),
                            Err(e) => error!("Update sensor failed {:?}", e),
                        }
                }
            }
            Err(diesel::result::Error::NotFound) => match diesel::insert_into(sensors)
                .values(&sensor_message)
                .execute(conn)
                {
                    Ok(_) => {
                        info!("Created {:?}", &sensor_message);
                        new_sensor_sender
                            .send((node.node_name, sensor_message))
                            .unwrap();
                    }
                    Err(e) => error!("Create sensor failed {:?}", e),
                },
            Err(e) => info!(
                "Error while checking for existing {:?} {:?}",
                &sensor_message, e
            ),
        }
}
