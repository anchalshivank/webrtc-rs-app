use tokio::time::sleep;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_VP8};
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, WebSocketStream};
use tungstenite::Message;

#[derive(Serialize, Deserialize)]
struct Signal {
    sdp_type: Option<String>, // "offer" | "answer"
    sdp: Option<String>,
    candidate: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (ws_stream, _) = connect_async("ws://127.0.0.1:8080").await?;
    let (ws_sender, mut ws_receiver) = ws_stream.split();
    let ws_sender = Arc::new(Mutex::new(ws_sender));

    let mut media_engine = MediaEngine::default();
    media_engine.register_default_codecs()?;

    let api = APIBuilder::new().with_media_engine(media_engine).build();

    let config = RTCConfiguration::default();
    let peer_connection = Arc::new(api.new_peer_connection(config).await?);

    let (offer_tx, mut offer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (candidate_tx, mut candidate_rx) = tokio::sync::mpsc::unbounded_channel();

    let is_offerer = std::env::args().nth(1).is_some();

    if is_offerer {
        let data_channel = peer_connection.create_data_channel("data", None).await?;

        let dc = Arc::clone(&data_channel);
        data_channel.on_open(Box::new(move || {
            Box::pin(async move {
                println!("Data channel '{}' opened", dc.label());
                loop {
                    dc.send_text("Hello from offerer!".to_string())
                    .await
                    .unwrap();
                            sleep(Duration::from_secs(1)).await;
                }
       
            })
        }));

        data_channel.on_message(Box::new(move |msg| {
            let msg_str = String::from_utf8(msg.data.to_vec()).unwrap();
            println!("Message from answerer: {}", msg_str);
            Box::pin(async {})
        }));

        let offer = peer_connection.create_offer(None).await?;
        peer_connection.set_local_description(offer).await?;

        let sdp = peer_connection.local_description().await.unwrap().sdp;
        let signal = Signal {
            sdp: Some(sdp),
            candidate: None,
            sdp_type: Some("offer".into()),
        };
        let payload = serde_json::to_string(&signal).unwrap();
        ws_sender.lock().await.send(Message::Text(payload)).await?;
    }

    peer_connection.on_ice_candidate(Box::new(move |candidate| {
        let candidate_tx = candidate_tx.clone();
        Box::pin(async move {
            if let Some(candidate) = candidate {
                let candidate_str = candidate.to_json().unwrap().candidate;
                let signal = Signal {
                    sdp: None,
                    candidate: Some(candidate_str),
                    sdp_type: None,
                };
                let payload = serde_json::to_string(&signal).unwrap();
                candidate_tx.send(Message::Text(payload)).unwrap();
            }
        })
    }));

    peer_connection.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
        println!("Peer Connection State has changed: {}", s);
        Box::pin(async {})
    }));

    if !is_offerer {
        peer_connection.on_data_channel(Box::new(move |dc| {
            let dc = Arc::new(dc);

            let dc_open = Arc::clone(&dc);
            dc_open.on_open(Box::new({
                let dc_open = Arc::clone(&dc_open); // ✅ clone outside closure
                move || {
                    Box::pin(async move {
                        println!("Data channel '{}' opened", dc_open.label());
                        loop {
             dc_open
                            .send_text("Hello from answerer!".to_string())
                            .await
                            .unwrap();

                                    sleep(Duration::from_secs(1)).await;
                        }
           
                    })
                }
            }));

            let dc_msg = Arc::clone(&dc);
            dc_msg.on_message(Box::new({
                let dc_msg = Arc::clone(&dc_msg); // ✅ clone outside closure
                move |msg| {
                    let msg_str = String::from_utf8(msg.data.to_vec()).unwrap();
                    println!("Message from offerer: {}", msg_str);
                    Box::pin(async {})
                }
            }));

            Box::pin(async {})
        }));
    }

    let pc = Arc::clone(&peer_connection);
    tokio::spawn(async move {
        while let Some(msg) = ws_receiver.next().await {
            if let Ok(Message::Text(text)) = msg {
                let signal: Signal = serde_json::from_str(&text).unwrap();
                if let Some(sdp) = signal.sdp {
                    if let Some(sdp_type) = signal.sdp_type.as_deref() {
                        let desc = match sdp_type {
                            "offer" => RTCSessionDescription::offer(sdp).unwrap(),
                            "answer" => RTCSessionDescription::answer(sdp).unwrap(),
                            _ => panic!("Invalid SDP type"),
                        };
                        pc.set_remote_description(desc).await.unwrap();

                        if !is_offerer && sdp_type == "offer" {
                            let answer = pc.create_answer(None).await.unwrap();
                            pc.set_local_description(answer).await.unwrap();
                            let sdp = pc.local_description().await.unwrap().sdp;
                            let signal = Signal {
                                sdp_type: Some("answer".into()),
                                sdp: Some(sdp),
                                candidate: None,
                            };
                            let payload = serde_json::to_string(&signal).unwrap();
                            offer_tx.send(Message::Text(payload)).unwrap();
                        }
                    }
                } else if let Some(candidate) = signal.candidate {
                    let candidate = RTCIceCandidateInit {
                        candidate,
                        ..Default::default()
                    };
                    pc.add_ice_candidate(candidate).await.unwrap();
                }
            }
        }
    });
    let ws_sender_offer = Arc::clone(&ws_sender);
    tokio::spawn(async move {
        while let Some(msg) = offer_rx.recv().await {
            ws_sender_offer.lock().await.send(msg).await.unwrap();
        }
    });

    let ws_sender_candidate = Arc::clone(&ws_sender);
    tokio::spawn(async move {
        while let Some(msg) = candidate_rx.recv().await {
            ws_sender_candidate.lock().await.send(msg).await.unwrap();
        }
    });

    tokio::signal::ctrl_c().await?;
    println!("Received Ctrl-C, shutting down");

    peer_connection.close().await?;

    Ok(())
}
