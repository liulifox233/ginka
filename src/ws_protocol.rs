// Copy from https://github.com/gwy15/biliapi/blob/main/src/ws_protocol.rs
// Author: [gwy15](https://github.com/gwy15)
// Modified by LiuliFox

//! bilibili 直播间传回来的数据

use std::fmt::{self, Debug, Display};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// 解析直播间数据时发生的错误
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("Websocket packet type not supported: {0}")]
    WsTypeNotSupported(String),

    #[error("IO error while parsing: {0}")]
    IO(#[from] std::io::Error),

    #[error("Encoding error: {0}")]
    Encoding(#[from] std::string::FromUtf8Error),
}

/// pure magic
pub mod magic {
    use serde::Deserialize;

    /// I    |      H    |  H  |  I  |   I
    /// u32  |     u16   | u16 | u32 |  u32
    /// size | head size | ver |  op | seq_id
    pub const HEADER_SIZE: usize = 4 + 2 + 2 + 4 + 4;

    pub const VER_ZLIB_COMPRESSED: u16 = 2;
    pub const VER_BROTLI_COMPRESSED: u16 = 3;
    pub const VER_NORMAL: u16 = 1;

    /// 已知的操作
    ///
    /// 从泄露代码的 app/service/main/broadcast/model/operation.go 可以看到命名
    #[enum_repr::EnumRepr(type = "u32")]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
    pub enum KnownOperation {
        Handshake = 0,
        HandshakeReply = 1,
        Heartbeat = 2,
        HeartbeatReply = 3,
        SendMsg = 4,
        SendMsgReply = 5,
        DisconnectReply = 6,
        Auth = 7,
        AuthReply = 8,
        Raw = 9,
        ProtoReady = 10,
        ProtoFinish = 11,
        ChangeRoom = 12,
        ChangeRoomReply = 13,
        Register = 14,
        RegisterReply = 15,
        Unregister = 16,
        UnregisterReply = 17,
    }
}

pub use magic::KnownOperation;
use tracing::{debug, trace, warn};

/// 每个 packet 对应的 operation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Known(magic::KnownOperation),
    Unknown(u32),
}
impl Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operation::Known(op) => f.write_fmt(format_args!("{:?}", op)),
            Operation::Unknown(op) => f.write_fmt(format_args!("Unknown({})", op)),
        }
    }
}
impl Serialize for Operation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for Operation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        match KnownOperation::deserialize(deserializer) {
            Ok(known) => Ok(Operation::Known(known)),
            Err(e) => {
                // TODO: implement
                Err(e)
            }
        }
    }
}
impl From<Operation> for u32 {
    fn from(op: Operation) -> u32 {
        match op {
            Operation::Known(k) => k as u32,
            Operation::Unknown(u) => u,
        }
    }
}
impl From<u32> for Operation {
    fn from(u: u32) -> Operation {
        match magic::KnownOperation::from_repr(u) {
            Some(k) => Self::Known(k),
            None => Self::Unknown(u),
        }
    }
}

/// 对应 websocket 返回的 packet，基本上没进行处理
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Packet {
    /// packet 对应的 operation，大部分应该都是 [`SendMsgReply`][`magic::KnownOperation::SendMsgReply`]
    pub operation: Operation,
    /// packet 对应的 数据，大部分都应该是 json string。这里不做任何解析
    pub body: String,
    /// 返回的包会带一个时间戳，表示收到时的时间戳，方便重放
    pub time: DateTime<Local>,
    /// 返回的包会带一个 room_id，表示收到的房间，方便重放
    pub room_id: u64,
}

impl Packet {
    /// 生成一个 auth 包
    pub fn auth(room_id: u64, token: &str, uid: i64) -> Self {
        let payload = serde_json::json!({
            "uid": uid,
            "roomid": room_id,
            "protover": 3,
            "platform": "android",
            "type": 2,
            "key": token
        });
        let body = serde_json::to_string(&payload).unwrap();

        Self {
            operation: Operation::Known(magic::KnownOperation::Auth),
            body,
            time: Local::now(),
            room_id,
        }
    }
    /// 生成一个心跳包
    pub fn heartbeat() -> Packet {
        Packet {
            operation: Operation::Known(magic::KnownOperation::Heartbeat),
            body: "{}".to_string(),
            time: Local::now(),
            room_id: 0,
        }
    }

    /// 从 bytes 解析出一堆 [`Packet`]
    pub fn from_bytes(bytes: &[u8], room_id: u64) -> Result<Vec<Packet>, ParseError> {
        use byteorder::{BigEndian, ReadBytesExt};
        use std::io::Read;

        let mut messages = vec![];
        // parse bytes to messages
        let mut buffer: &[u8] = bytes;
        while !buffer.is_empty() {
            // 见 magic::HEADER_SIZE
            trace!("parsing header, buffer size = {:?} bytes", buffer.len());
            if buffer.len() < magic::HEADER_SIZE {
                debug!("header too small, ignore: {:2x?}", buffer);
                break;
            }
            let total_size = buffer.read_u32::<BigEndian>()?;
            let _raw_header_size = buffer.read_u16::<BigEndian>()?;
            let ver = buffer.read_u16::<BigEndian>()?;
            let operation = buffer.read_u32::<BigEndian>()?;
            let operation = Operation::from(operation);
            let seq_id = buffer.read_u32::<BigEndian>()?;
            trace!("header parsed, seq_id = {}", seq_id);
            // read rest data
            let offset = total_size as usize - magic::HEADER_SIZE;

            let body_buffer = &buffer[..offset];

            match (operation, ver) {
                (_, magic::VER_ZLIB_COMPRESSED) => {
                    trace!(
                        "ver = VER_ZLIB_COMPRESSED, op = {:?}, trying decompress",
                        operation
                    );
                    let mut z = flate2::read::ZlibDecoder::new(body_buffer);
                    let mut buffer = vec![];
                    let bytes_read = z.read_to_end(&mut buffer)?;
                    trace!("read {} bytes from zlib", bytes_read);
                    // 居然还要递归
                    let sub_messages = Self::from_bytes(&buffer, room_id)
                        .map_err(|e| match e {
                            ParseError::Encoding(e) => {
                                debug!("utf8 decoded error, raw bytes = {:?}", bytes);
                                e.into()
                            }
                            e => e,
                        })
                        .unwrap();

                    trace!("zlib sub_messages: {:?}", sub_messages);
                    messages.extend(sub_messages);
                }
                (_, magic::VER_BROTLI_COMPRESSED) => {
                    trace!(
                        "ver = VER_BROTLI_COMPRESSED, op = {:?}, trying decompress",
                        operation
                    );
                    let mut decompressor =
                        brotli::Decompressor::new(body_buffer, body_buffer.len());
                    let mut buffer = vec![];
                    let bytes_read = decompressor.read_to_end(&mut buffer)?;
                    trace!("read {} bytes from brotli", bytes_read);

                    let sub_messages = Self::from_bytes(&buffer, room_id)
                        .map_err(|e| match e {
                            ParseError::Encoding(e) => {
                                debug!("utf8 decoded error, raw bytes = {:?}", bytes);
                                e.into()
                            }
                            e => e,
                        })
                        .unwrap();

                    trace!("brotli sub_messages: {:?}", sub_messages);
                    messages.extend(sub_messages);
                }
                (Operation::Known(magic::KnownOperation::HeartbeatReply), magic::VER_NORMAL) => {
                    // 烦不烦，能不能统一返回 string
                    let mut body_buffer = body_buffer;
                    let popularity = body_buffer.read_u32::<BigEndian>()?;
                    debug!("got a heartbeat response: {}", popularity);
                    let message = Packet {
                        operation,
                        body: popularity.to_string(),
                        time: Local::now(),
                        room_id,
                    };
                    messages.push(message);
                }
                (operation, ver) => {
                    let body = match String::from_utf8(body_buffer.to_vec()) {
                        Ok(body) => body,
                        Err(e) => {
                            debug!("utf8 decoded error, raw bytes = {:?}", bytes);
                            warn!(
                                "Failed to parse body as utf8, op = {:?}, ver = {:?}",
                                operation, ver
                            );
                            return Err(e.into());
                        }
                    };

                    let message = Packet {
                        operation,
                        body,
                        time: Local::now(),
                        room_id,
                    };
                    messages.push(message);
                }
            }

            buffer = &buffer[offset..];
        }
        Ok(messages)
    }

    /// 从 [`WsMessage`] 解析出一堆 [`Packet`]
    pub fn from_ws_message(ws_message: WsMessage, room_id: u64) -> Result<Vec<Packet>, ParseError> {
        match ws_message {
            WsMessage::Binary(bytes) => Self::from_bytes(&bytes, room_id),
            WsMessage::Ping(_) => {
                debug!("received a ping message, ignore");
                Ok(vec![])
            }
            ws_message => {
                warn!("Unknown type of websocket message: {:?}", ws_message);
                Err(ParseError::WsTypeNotSupported(ws_message.to_string()))
            }
        }
    }
}

impl From<Packet> for WsMessage {
    fn from(msg: Packet) -> WsMessage {
        use byteorder::{BigEndian, WriteBytesExt};

        let body_size = msg.body.len();
        let total_size = magic::HEADER_SIZE + body_size;

        let mut buffer = vec![0; magic::HEADER_SIZE];
        buffer.extend_from_slice(msg.body.as_bytes());

        let mut cursor = std::io::Cursor::new(buffer);

        cursor.write_u32::<BigEndian>(total_size as u32).unwrap();
        cursor
            .write_u16::<BigEndian>(magic::HEADER_SIZE as u16)
            .unwrap();
        cursor.write_u16::<BigEndian>(1u16).unwrap();
        cursor.write_u32::<BigEndian>(msg.operation.into()).unwrap();
        cursor.write_u32::<BigEndian>(1u32).unwrap();

        let bytes = cursor.into_inner();
        WsMessage::Binary(bytes.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_operation_serialize() {
        use serde_json::json;
        assert_eq!(
            serde_json::to_string(&json!({
                "op": Operation::Known(KnownOperation::SendMsgReply)
            }))
            .unwrap(),
            r#"{"op":"SendMsgReply"}"#
        );
        assert_eq!(
            serde_json::to_string(&json!({
                "op": Operation::Unknown(114514)
            }))
            .unwrap(),
            r#"{"op":"Unknown(114514)"}"#
        );
    }

    #[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
    struct Test {
        op: Operation,
    }

    #[test]
    fn test_operation_deserialize_known() {
        assert_eq!(
            serde_json::from_str::<Test>(r#"{"op":"SendMsgReply"}"#).unwrap(),
            Test {
                op: Operation::Known(KnownOperation::SendMsgReply)
            }
        );
    }

    #[test]
    #[ignore = "not yet implemented"]
    fn test_operation_deserialize_unknown() {
        assert_eq!(
            serde_json::from_str::<Test>(r#"{"op":"Unknown(114514)"}"#).unwrap(),
            Test {
                op: Operation::Unknown(114514)
            }
        );
    }
}
