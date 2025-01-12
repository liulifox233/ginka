mod model;
mod notice;
mod ws_protocol;

use crate::ws_protocol::Operation::{Known, Unknown};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use mac_notification_sys::{get_bundle_identifier_or_default, send_notification, set_application};
use std::process::Command;
use std::sync::OnceLock;

use tokio_tungstenite::connect_async;
use tracing::{debug, info, warn};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Arg {
    /// Room ID
    room_id: u64,

    /// Live mode
    #[arg(short, long)]
    live: bool,
}

static COOKIE: OnceLock<String> = OnceLock::new();

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    tracing::info!("Starting application...");

    COOKIE
        .set(std::env::var("COOKIE").expect("COOKIE must be set"))
        .unwrap();

    let bundle = get_bundle_identifier_or_default("finder");
    set_application(&bundle).unwrap();

    let arg = Arg::parse();

    let room_id = arg.room_id;

    match arg.live {
        true => live(room_id).await,
        false => watch(room_id).await,
    }
}

async fn live(room_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let cookie = COOKIE.get().unwrap();
    let start_live_reply = reqwest::Client::new()
        .post("https://api.live.bilibili.com/room/v1/Room/startLive")
        .header("Cookie", cookie.to_string())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "room_id={}&area_v2={}&platform={}&csrf={}",
            room_id,
            372,
            "android_link",
            cookie
                .split(';')
                .find_map(|s| {
                    let mut parts = s.split('=');
                    if parts.next()?.trim() == "bili_jct" {
                        parts.next().map(|v| v.trim().to_string())
                    } else {
                        None
                    }
                })
                .expect("CSRF token not found in COOKIE")
        ))
        .send()
        .await?
        .json::<model::StartLiveReply>()
        .await?;

    let rtmp_addr = start_live_reply.data.rtmp.addr;
    let rtmp_code = start_live_reply.data.rtmp.code;

    info!("rtmp address: {}{}", rtmp_addr, rtmp_code);
    info!("rtmp code: {}", rtmp_code);

    let (shutdown_tx, mut _shutdown_rx) = tokio::sync::mpsc::channel(1);

    info!("Application started");

    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for ctrl+c");
    shutdown_tx
        .send(())
        .await
        .expect("Failed to send shutdown signal");
    info!("Received Ctrl+C, shutting down...");
    let res = reqwest::Client::new()
        .post("https://api.live.bilibili.com/room/v1/Room/stopLive")
        .header("Cookie", cookie.to_string())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "room_id={}&csrf={}",
            room_id,
            cookie
                .split(';')
                .find_map(|s| {
                    let mut parts = s.split('=');
                    if parts.next()?.trim() == "bili_jct" {
                        parts.next().map(|v| v.trim().to_string())
                    } else {
                        None
                    }
                })
                .expect("CSRF token not found in COOKIE")
        ))
        .send()
        .await
        .unwrap();
    info!("Stop live: {:?}", res.json::<serde_json::Value>().await);
    Ok(())
}

async fn watch(room_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let connect_addr =
        format!("https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo?id={room_id}");
    let cookie = COOKIE.get().unwrap();

    let uid = cookie
        .split(';')
        .find_map(|s| {
            let mut parts = s.split('=');
            if parts.next()?.trim() == "DedeUserID" {
                parts.next().map(|v| v.trim().to_string())
            } else {
                None
            }
        })
        .expect("DedeUserID not found in COOKIE")
        .parse::<i64>()
        .expect("DedeUserID parse error");

    let room_info = reqwest::Client::new()
        .get(connect_addr)
        .header("Cookie", cookie.to_string())
        .send()
        .await?
        .json::<model::RoomDanmuInfo>()
        .await?;

    let token = room_info.data.token;
    let host_list = room_info.data.host_list;

    let host = host_list[2].clone();
    let (mut ws_stream, _) = connect_async({
        let mut request =
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
                format!("ws://{}:{}/sub", host.host, host.ws_port),
            )?;
        request.headers_mut().insert(
        "User-Agent",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.2 Safari/605.1.15".parse().unwrap(),
        );
        request
    })
    .await
    .unwrap();

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::from(
            ws_protocol::Packet::auth(room_id, &token, uid),
        ))
        .await
        .unwrap();

    let (ws_write, mut ws_read) = ws_stream.split();
    tokio::spawn(async move {
        let mut ws_write = ws_write;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
            ws_write
                .send(tokio_tungstenite::tungstenite::Message::from(
                    ws_protocol::Packet::heartbeat(),
                ))
                .await
                .unwrap();
        }
    });

    info!("Application started");

    while let Some(msg) = ws_read.next().await {
        match msg {
            Ok(msg) => {
                let pockets = ws_protocol::Packet::from_ws_message(msg, room_id).unwrap();
                for pocket in pockets {
                    match pocket.operation {
                        Known(crate::ws_protocol::magic::KnownOperation::SendMsgReply)
                        | Known(crate::ws_protocol::magic::KnownOperation::SendMsg) => {
                            let deserialized: serde_json::Value =
                                serde_json::from_str(&pocket.body).unwrap();
                            match deserialized["cmd"].to_string().as_str() {
                                "\"DANMU_MSG\"" => {
                                    let info: serde_json::Value =
                                        serde_json::from_str(&deserialized["info"].to_string())
                                            .unwrap();

                                    let comment_text = info[1].as_str().unwrap_or("");
                                    let _user_id = info[2][0].as_i64().unwrap_or(0);
                                    let user_name = info[2][1].as_str().unwrap_or("");
                                    let _is_admin = info[2][2].as_i64().unwrap_or(0) == 1;
                                    let _is_vip = info[2][3].as_i64().unwrap_or(0) == 1;
                                    let _user_guard_level = info[7].as_i64().unwrap_or(0);
                                    debug!("{:?}", info[0][15]);
                                    let avatar =
                                        info[0][15]["user"]["base"]["face"].as_str().unwrap();

                                    debug!(avatar);

                                    let message =
                                        format!("User: {}, Comment: {}", user_name, comment_text);
                                    info!("{message}");
                                    Command::new("terminal-notifier")
                                        .args([
                                            "-title",
                                            user_name,
                                            "-message",
                                            comment_text,
                                            "-contentImage",
                                            avatar,
                                        ])
                                        .output()
                                        .expect("Failed to execute command");
                                }
                                "\"INTERACT_WORD\"" => {
                                    let data: serde_json::Value =
                                        serde_json::from_str(&deserialized["data"].to_string())
                                            .unwrap();
                                    match data["msg_type"].as_i64() {
                                        Some(1) => {
                                            let uname = data["uname"].as_str().unwrap();
                                            info!("{uname} 进入直播间");
                                            send_notification(uname, None, "进入直播间", None)
                                                .unwrap();
                                        }
                                        Some(2) => {
                                            let uname = data["uname"].as_str().unwrap();
                                            info!("{uname} 关注了主播");
                                            send_notification(uname, None, "关注了主播", None)
                                                .unwrap();
                                        }
                                        _ => {}
                                    }
                                }
                                c => {
                                    debug!("other cmd: {c} => {deserialized:?}");
                                }
                            }
                        }
                        Known(m) => {
                            debug!("KNOWN msg {:?}", m);
                        }
                        Unknown(m) => {
                            info!("UNKNOWN msg: {}", m);
                        }
                    }
                }
            }
            Err(e) => {
                warn!("err: {e:?}");
            }
        }
    }
    Ok(())
}
