use std::{
    collections::HashMap,
    sync::{mpsc::Sender, Arc, Mutex},
};

use crate::{
    connect::{ConnectionError, ConnectionRead},
    errors::LoopControlError,
    events::IrcEvent,
    handler::Handler,
    types::{
        Channel, ErrorType, JoinMsg, JoinReply, Message, Nick, NickMsg, ParsedMessage, PartMsg,
        PartReply, PrivMsg, PrivReply, QuitMsg, QuitReply, Reply, Target, UnparsedMessage, UserMsg,
        WelcomeReply,
    },
};

pub struct Client {
    pub nick: Option<Nick>,
    pub user: Option<String>,
    conn_read: ConnectionRead,
    conn_write: Sender<IrcEvent>,
    clients: Arc<Mutex<HashMap<Nick, Sender<IrcEvent>>>>,
    channels: Arc<Mutex<HashMap<Channel, HashMap<Nick, Sender<IrcEvent>>>>>,
}

impl Client {
    pub fn new(
        conn_read: ConnectionRead,
        conn_write: Sender<IrcEvent>,
        clients: Arc<Mutex<HashMap<Nick, Sender<IrcEvent>>>>,
        channels: Arc<Mutex<HashMap<Channel, HashMap<Nick, Sender<IrcEvent>>>>>,
    ) -> Self {
        Self {
            conn_read,
            conn_write,
            clients,
            channels,
            nick: None,
            user: None,
        }
    }

    pub fn rid(&self) -> String {
        self.conn_read.id()
    }

    pub fn send(&mut self, message: String) {
        self.conn_write.send(IrcEvent::Send(message)).unwrap();
    }

    pub fn terminate(&mut self) {
        self.conn_write.send(IrcEvent::Terminate).unwrap();
    }

    pub fn recv(&mut self) -> Result<String, LoopControlError> {
        self.conn_read.read_message().map_err(|e| match e {
            ConnectionError::ConnectionLost | ConnectionError::ConnectionClosed => {
                log::error!("{}# Connection lost", self.rid());
                LoopControlError::Break
            }
            _ => {
                log::error!("{}# Invalid message received... ignoring", self.rid());
                LoopControlError::Continue
            }
        })
    }

    pub fn parse(&mut self, message: String) -> Result<ParsedMessage, LoopControlError> {
        ParsedMessage::try_from(UnparsedMessage {
            message: &message,
            // use a dummy nickname if client not logged in yet
            sender_nick: self
                .nick
                .clone()
                .unwrap_or_else(|| Nick(String::from("Person"))),
        })
        .map_err(|e| {
            self.send(format!("{e}\r\n"));
            log::error!("{}# {e}", self.rid());
            LoopControlError::Continue
        })
    }

    pub fn handle_message(
        &mut self,
        parsed_message: ParsedMessage,
    ) -> Result<(), LoopControlError> {
        match parsed_message.message.clone() {
            Message::Nick(nick_msg) => self.handle(nick_msg),
            Message::User(user_msg) => self.handle(user_msg),
            Message::PrivMsg(priv_msg) => self.handle(priv_msg),
            Message::Ping(s) => self.handle(s),
            Message::Join(join_msg) => self.handle(join_msg),
            Message::Part(part_msg) => self.handle(part_msg),
            Message::Quit(quit_msg) => self.handle(quit_msg),
        }

        if let Message::Quit(_) = parsed_message.message {
            Err(LoopControlError::Break)
        } else {
            Ok(())
        }
    }

    pub fn login(&mut self) -> Option<Nick> {
        loop {
            // wait for message
            let message = match self.recv() {
                Ok(message) => message,
                Err(LoopControlError::Break) => break,
                Err(LoopControlError::Continue) => continue,
            };

            log::info!("{}# Received message: {message}", self.rid());

            // parse the received message
            let parsed_message = match self.parse(message) {
                Ok(parsed_message) => parsed_message,
                Err(LoopControlError::Break) => break, // should not happen
                Err(LoopControlError::Continue) => continue,
            };

            // handle parsed message
            match parsed_message.message {
                Message::Nick(nick_msg) => self.handle(nick_msg),
                Message::User(user_msg) => self.handle(user_msg),
                Message::Quit(_) => self.terminate(),
                _ => {
                    // self.send("Expected NICK or USER command... ignoring\r\n".to_string());
                    log::warn!("{}# Expected NICK or USER command... ignoring", self.rid());
                }
            }

            // check if logged in
            if self.nick.is_some() && self.user.is_some() {
                self.welcome();
                return Some(self.nick.as_ref().unwrap().clone());
            }
        }

        None
    }

    fn welcome(&mut self) {
        // send welcome message
        self.send(
            Reply::Welcome(WelcomeReply {
                target_nick: self.nick.clone().unwrap(),
                message: format!("Hi {}, welcome to IRC", self.user.clone().unwrap()),
            })
            .to_string(),
        );

        log::info!(
            "{}# {} ({}) joined",
            self.rid(),
            self.user.clone().unwrap(),
            self.nick.clone().unwrap()
        );
    }
}

impl Handler<NickMsg> for Client {
    type Result = ();

    fn handle(&mut self, message: NickMsg) -> Self::Result {
        if self.clients.lock().unwrap().contains_key(&message.nick) {
            log::info!("Nickname already taken: {}", message.nick);
            self.send(format!("{}\r\n", ErrorType::NickCollision.to_string()));
        } else {
            if self.nick.is_none() {
                self.nick = Some(message.nick);

                log::debug!(
                    "{}# Nickname set: {}",
                    self.rid(),
                    self.nick.clone().unwrap()
                );
            }
        }
    }
}

impl Handler<UserMsg> for Client {
    type Result = ();

    fn handle(&mut self, message: UserMsg) -> Self::Result {
        if self.user.is_none() {
            self.user = Some(message.real_name);

            log::debug!(
                "{}# Username set: {}",
                self.rid(),
                self.user.clone().unwrap()
            );
        }
    }
}

// Ping
impl Handler<String> for Client {
    type Result = ();

    fn handle(&mut self, message: String) -> Self::Result {
        self.send(Reply::Pong(message).to_string());
    }
}

impl Handler<PrivMsg> for Client {
    type Result = ();

    fn handle(&mut self, message: PrivMsg) -> Self::Result {
        match message.target.clone() {
            Target::User(nick) => {
                // pm to user
                if let Some(client) = self.clients.clone().lock().unwrap().get_mut(&nick) {
                    client
                        .send(IrcEvent::Send(
                            Reply::PrivMsg(PrivReply {
                                message,
                                sender_nick: self.nick.clone().unwrap(),
                            })
                            .to_string(),
                        ))
                        .unwrap();
                } else {
                    // no such nick
                    self.send(format!("{}\r\n", ErrorType::NoSuchNick.to_string()));
                };
            }
            Target::Channel(channel) => {
                // pm to channel
                if let Some(members) = self.channels.clone().lock().unwrap().get(&channel) {
                    members.into_iter().for_each(|(nick, sender)| {
                        if *nick != self.nick.clone().unwrap() {
                            sender
                                .send(IrcEvent::Send(
                                    Reply::PrivMsg(PrivReply {
                                        message: message.clone(),
                                        sender_nick: self.nick.clone().unwrap(),
                                    })
                                    .to_string(),
                                ))
                                .unwrap();
                        }
                    });
                } else {
                    // no such channel
                    self.send(format!("{}\r\n", ErrorType::NoSuchChannel.to_string()));
                };
            }
        }
    }
}

impl Handler<JoinMsg> for Client {
    type Result = ();

    fn handle(&mut self, message: JoinMsg) -> Self::Result {
        self.channels
            .lock()
            .unwrap()
            .entry(message.channel.clone())
            .and_modify(|members| {
                // channel exists
                members.insert(self.nick.clone().unwrap(), self.conn_write.clone());
            })
            .or_insert_with(|| {
                // new channel
                log::info!("New channel created: {}", message.channel);
                let mut new_members = HashMap::new();
                new_members.insert(self.nick.clone().unwrap(), self.conn_write.clone());
                new_members
            });

        log::info!(
            "User {} joined channel {}",
            self.nick.clone().unwrap(),
            message.channel
        );

        if let Some(channel) = self.channels.lock().unwrap().get(&message.channel) {
            channel.into_iter().for_each(|(_, sender)| {
                sender
                    .send(IrcEvent::Send(
                        Reply::Join(JoinReply {
                            message: message.clone(),
                            sender_nick: self.nick.clone().unwrap(),
                        })
                        .to_string(),
                    ))
                    .unwrap();
            })
        }

        log::debug!("Channels: {:?}", self.channels);
    }
}

impl Handler<PartMsg> for Client {
    type Result = ();

    fn handle(&mut self, message: PartMsg) -> Self::Result {
        if let Some(channel) = self.channels.lock().unwrap().get_mut(&message.channel) {
            if let Some(_) = channel.remove(&self.nick.clone().unwrap()) {
                // channel exists & user was in channel
                // send message to other users
                channel.into_iter().for_each(|(_, sender)| {
                    sender
                        .send(IrcEvent::Send(
                            Reply::Part(PartReply {
                                message: message.clone(),
                                sender_nick: self.nick.clone().unwrap(),
                            })
                            .to_string(),
                        ))
                        .unwrap();
                });

                log::info!(
                    "User {} left channel {}",
                    self.nick.clone().unwrap(),
                    message.channel
                );
            }
        }

        // remove channel if no more members
        let mut guard = self.channels.lock().unwrap();
        if let None = guard.get(&message.channel).and_then(|channel| {
            if channel.is_empty() {
                None
            } else {
                Some(channel)
            }
        }) {
            log::info!("Deleting channel: {}", message.channel);
            guard.remove(&message.channel);
        }

        drop(guard);
        log::debug!("Channels: {:?}", self.channels);
    }
}

impl Handler<QuitMsg> for Client {
    type Result = ();

    fn handle(&mut self, message: QuitMsg) -> Self::Result {
        let mut empty_channels = Vec::new();

        self.channels
            .lock()
            .unwrap()
            .iter_mut()
            .for_each(|(channel_name, channel)| {
                if let Some(_) = channel.get(&self.nick.clone().unwrap()) {
                    // user is leaving this channel
                    channel.iter().for_each(|(_, sender)| {
                        sender
                            .send(IrcEvent::Send(
                                Reply::Quit(QuitReply {
                                    message: message.clone(),
                                    sender_nick: self.nick.clone().unwrap(),
                                })
                                .to_string(),
                            ))
                            .unwrap();
                    });

                    channel.remove(&self.nick.clone().unwrap());
                    if channel.is_empty() {
                        empty_channels.push(channel_name.clone());
                    }

                    log::info!(
                        "User {} quit and left channel {}",
                        self.nick.clone().unwrap(),
                        channel_name
                    );
                }
            });

        empty_channels.into_iter().for_each(|ch| {
            log::info!("Channel {ch} is now empty... deleting");
            self.channels.lock().unwrap().remove(&ch);
        });

        log::debug!("Channels: {:?}", self.channels);
    }
}
