use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tungstenite::Message;

type PeerMap = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Message>>>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(&addr).await?;
    println!("Signaling server listening on: {}", addr);

    let state = PeerMap::new(Mutex::new(HashMap::new()));

    while let Ok((stream, _)) = listener.accept().await {
        let peer_map = state.clone();
        tokio::spawn(handle_connection(peer_map, stream));
    }

    Ok(())
}

async fn handle_connection(peer_map: PeerMap, raw_stream: TcpStream) {
    let ws_stream = accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake");
    println!("New WebSocket connection");

    let (tx, mut rx) = mpsc::unbounded_channel();
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    let peer_id = uuid::Uuid::new_v4().to_string();
    peer_map.lock().unwrap().insert(peer_id.clone(), tx);

    // Forward messages from the WebSocket to the other peer
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            ws_sender.send(msg).await.unwrap();
        }
    });

    // Handle incoming messages from the WebSocket
    while let Some(Ok(msg)) = ws_receiver.next().await {
        let peers = peer_map.lock().unwrap();
        for (id, peer_tx) in peers.iter() {
            if *id != peer_id {
                peer_tx.send(msg.clone()).unwrap();
            }
        }
    }

    println!("{} disconnected", peer_id);
    peer_map.lock().unwrap().remove(&peer_id);
}
