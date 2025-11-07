use media_protocol::{ClientError, Command, Deck, Response, ResponseData};
use nng::{Protocol, Socket};
use std::path::PathBuf;

pub struct MediaClient {
    socket: Socket,
}

impl MediaClient {
    pub fn connect(url: &str) -> Result<Self, ClientError> {
        let socket =
            Socket::new(Protocol::Req0).map_err(|e| ClientError::Connection(format!("{:?}", e)))?;

        socket
            .dial(url)
            .map_err(|e| ClientError::Connection(format!("{:?}", e)))?;

        Ok(Self { socket })
    }

    pub fn load_track(&self, path: PathBuf, deck: Deck) -> Result<(), ClientError> {
        let cmd = Command::LoadTrack { path, deck };
        self.send_command(cmd)
    }

    pub fn stop(&self, deck: Deck) -> Result<(), ClientError> {
        let cmd = Command::Stop { deck };
        self.send_command(cmd)
    }

    pub fn set_volume(&self, deck: Deck, db: f32) -> Result<(), ClientError> {
        let cmd = Command::SetVolume { deck, db };
        self.send_command(cmd)
    }

    pub fn unload_track(&self, deck: Deck) -> Result<(), ClientError> {
        let cmd = Command::Unload { deck };
        self.send_command(cmd)
    }

    pub fn play(&self, deck: Deck) -> Result<(), ClientError> {
        tracing::info!("Client: Sending Play command for deck {:?}", deck);
        let cmd = Command::Play { deck };
        let result = self.send_command(cmd);
        tracing::info!("Client: Play command result: {:?}", result);
        result
    }

    fn send_command(&self, cmd: Command) -> Result<(), ClientError> {
        println!("Client: Serializing command: {:?}", cmd);
        let data = serde_json::to_vec(&cmd).map_err(|e| ClientError::Protocol(e.to_string()))?;
        println!("Client: Sending {} bytes to server", data.len());

        match self.socket.send(&data) {
            Ok(_) => println!("Client: Command sent successfully"),
            Err(e) => return Err(ClientError::Connection(format!("Send error: {:?}", e))),
        }

        println!("Client: Waiting for response...");
        let msg = match self.socket.recv() {
            Ok(msg) => {
                println!("Client: Received response of {} bytes", msg.len());
                msg
            }
            Err(e) => return Err(ClientError::Connection(format!("Receive error: {:?}", e))),
        };

        println!("Client: Deserializing response");
        let response: Response =
            serde_json::from_slice(&msg).map_err(|e| ClientError::Protocol(e.to_string()))?;

        if !response.success {
            return Err(ClientError::Command(response.error_message));
        }

        Ok(())
    }
    pub fn seek(&self, deck: Deck, position: usize) -> Result<(), ClientError> {
        let cmd = Command::Seek { deck, position };
        self.send_command(cmd)
    }

    // New helper method for commands that return data
    fn send_command_with_response<T>(
        &self,
        cmd: Command,
        extract: fn(ResponseData) -> Option<T>,
    ) -> Result<T, ClientError> {
        let data = serde_json::to_vec(&cmd).map_err(|e| ClientError::Protocol(e.to_string()))?;

        self.socket
            .send(&data)
            .map_err(|e| ClientError::Connection(format!("{:?}", e)))?;

        let msg = self
            .socket
            .recv()
            .map_err(|e| ClientError::Connection(format!("{:?}", e)))?;

        let response: Response =
            serde_json::from_slice(&msg).map_err(|e| ClientError::Protocol(e.to_string()))?;

        if !response.success {
            return Err(ClientError::Command(response.error_message));
        }

        match response.data {
            Some(data) => {
                if let Some(result) = extract(data) {
                    Ok(result)
                } else {
                    Err(ClientError::Protocol(
                        "Unexpected response data type".to_string(),
                    ))
                }
            }
            None => Err(ClientError::Protocol("Missing response data".to_string())),
        }
    }

    pub fn get_length(&self, deck: Deck) -> Result<usize, ClientError> {
        let cmd = Command::GetLength { deck };

        self.send_command_with_response(cmd, |data| {
            if let ResponseData::Length(len) = data {
                Some(len)
            } else {
                None
            }
        })
    }
}
