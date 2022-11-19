pub mod client;
pub mod connect;
pub mod errors;
pub mod events;
pub mod handler;
pub mod types;

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        mpsc::{self, Sender},
        Arc, Mutex,
    },
    thread,
};

use client::Client;
use connect::{ConnectionRead, ConnectionWrite};
use types::{Channel, Nick};

use crate::{
    connect::ConnectionManager, errors::LoopControlError, events::IrcEvent, types::SERVER_NAME,
};

pub struct Iris {
    ip_address: IpAddr,
    port: u16,
    clients: Arc<Mutex<HashMap<Nick, Sender<IrcEvent>>>>,
    channels: Arc<Mutex<HashMap<Channel, HashMap<Nick, Sender<IrcEvent>>>>>,
}

impl Iris {
    pub fn new(ip_address: IpAddr, port: u16) -> Self {
        Self {
            ip_address,
            port,
            clients: Arc::new(Mutex::new(HashMap::new())),
            channels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn start(&self) {
        thread::scope(|scope| {
            log::info!(
                "Launching {} at {}:{}",
                SERVER_NAME,
                self.ip_address,
                self.port
            );

            // accept loop
            scope.spawn(move || {
                let mut connection_manager = ConnectionManager::launch(self.ip_address, self.port);
                loop {
                    let (conn_read, conn_write) = connection_manager.accept_new_connection();
                    log::info!("{}# Connection established", conn_read.id());
                    scope.spawn(|| self.handle_connection(conn_read, conn_write));
                }
            });
        });
    }

    fn handle_connection(&self, conn_read: ConnectionRead, mut conn_write: ConnectionWrite) {
        let (tx, rx) = mpsc::channel::<IrcEvent>();
        let mut client = Client::new(
            conn_read,
            tx.clone(),
            self.clients.clone(),
            self.channels.clone(),
        );
        let clients = self.clients.clone();

        // thread for reading and handling messages
        // messages are handled by sending (through a channel) a server reply to the write loop thread where the reply is sent
        let read_loop_handle = thread::spawn(move || {
            let nick = match client.login() {
                Some(nick) => nick,
                None => {
                    client.terminate();
                    return; // connection lost during login
                }
            };
            clients.lock().unwrap().insert(nick, tx.clone());

            loop {
                // wait for message
                let message = match client.recv() {
                    Ok(message) => message,
                    Err(LoopControlError::Break) => break,
                    Err(LoopControlError::Continue) => continue,
                };

                log::info!("{}# Received message: {message}", client.rid());

                // parse the received message
                let parsed_message = match client.parse(message) {
                    Ok(parsed_message) => parsed_message,
                    Err(LoopControlError::Break) => break, // should not happen
                    Err(LoopControlError::Continue) => continue,
                };

                // handle parsed message
                if let Err(LoopControlError::Break) = client.handle_message(parsed_message) {
                    break;
                }
            }

            client.terminate();
        });

        // thread for sending server replies
        let write_loop_handle = thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                match event {
                    IrcEvent::Send(message) => conn_write.write_message(&message).unwrap(),
                    IrcEvent::Terminate => break,
                }
            }
        });

        read_loop_handle.join().unwrap();
        write_loop_handle.join().unwrap();
        log::debug!("Thread finished");
    }
}
