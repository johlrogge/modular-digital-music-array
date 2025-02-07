use crate::error::ClientError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Channel {
    A,
    B,
}

impl TryFrom<i32> for Channel {
    type Error = ClientError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Channel::A),
            1 => Ok(Channel::B),
            _ => Err(ClientError::Protocol("Invalid channel value".into())),
        }
    }
}

impl From<Channel> for i32 {
    fn from(channel: Channel) -> Self {
        match channel {
            Channel::A => 0,
            Channel::B => 1,
        }
    }
}
