use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RoomDanmuInfo {
    pub code: i32,
    pub message: String,
    pub ttl: i32,
    pub data: RoomDanmuInfoData,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RoomDanmuInfoData {
    pub group: String,
    pub business_id: i32,
    pub refresh_row_factor: f32,
    pub refresh_rate: i32,
    pub max_delay: i32,
    pub token: String,
    pub host_list: Vec<RoomDanmuInfoDataHostList>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RoomDanmuInfoDataHostList {
    pub host: String,
    pub port: i32,
    pub wss_port: i32,
    pub ws_port: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StartLiveReply {
    pub code: i32,
    pub message: String,
    pub data: StartLiveReplyData,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StartLiveReplyData {
    pub rtmp: Rtmp,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Rtmp {
    pub addr: String,
    pub code: String,
}
