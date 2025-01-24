use warp::Filter;
use std::sync::{Arc, Mutex};
use std::collections::HashSet;
use futures::{StreamExt, sink::SinkExt};
use warp::ws::{Message, WebSocket};
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::wrappers::UnboundedReceiverStream;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub username: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    AddMessage(ChatMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    FullHistory(Vec<ChatMessage>),
    System(String),
}

struct ServerState {
    clients: Vec<UnboundedSender<Message>>,
    logs: Vec<ChatMessage>,
    active_users: HashSet<String>,
}

type State = Arc<Mutex<ServerState>>;

#[tokio::main]
async fn main() {
    let state: State = Arc::new(Mutex::new(ServerState {
        clients: Vec::new(),
        logs: Vec::new(),
        active_users: HashSet::new(),
    }));

    let ws_route = warp::path("ws")
        .and(warp::ws())
        .and(with_state(state.clone()))
        .map(|ws: warp::ws::Ws, state| {
            ws.on_upgrade(move |socket| handle_connection(socket, state))
        });

    println!("Server in ascolto su http://127.0.0.1:9001/ws");
    warp::serve(ws_route)
        .run(([127, 0, 0, 1], 9001))
        .await;
}

fn with_state(state: State)
    -> impl Filter<Extract = (State,), Error = std::convert::Infallible> + Clone
{
    warp::any().map(move || state.clone())
}

async fn handle_connection(ws: WebSocket, state: State) {
    let (mut tx, mut rx) = ws.split();
    let (client_sender, client_rcv) = tokio::sync::mpsc::unbounded_channel::<Message>();

    let current_history: Option<Vec<ChatMessage>>;
    {
        let mut guard = state.lock().unwrap();
        guard.clients.push(client_sender);

        if !guard.logs.is_empty() {
            current_history = Some(guard.logs.clone());
        } else {
            current_history = None;
        }

        println!("Nuovo client. Totale client: {}", guard.clients.len());
        // Esempio: stampiamo anche la dimensione di active_users (se c’è)
        println!("(DEBUG) Numero di utenti attivi finora: {}", guard.active_users.len());
    }

    if let Some(h) = current_history {
        let full_msg = ServerMessage::FullHistory(h);
        let text = serde_json::to_string(&full_msg).unwrap();

        let guard = state.lock().unwrap();
        if let Some(last) = guard.clients.last() {
            let _ = last.send(Message::text(text));
        }
    }

    let mut stream = UnboundedReceiverStream::new(client_rcv);
    let send_task = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            if tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = rx.next().await {
        if msg.is_text() {
            if let Ok(txt) = msg.to_str() {
                if let Ok(cm) = serde_json::from_str::<ClientMessage>(&txt) {
                    match cm {
                        ClientMessage::AddMessage(chat_msg) => {
                            println!("AddMessage => {}: {}", chat_msg.username, chat_msg.message);
                            // Memorizziamo e aggiorniamo tutti
                            store_and_broadcast(state.clone(), chat_msg).await;
                        }
                    }
                } else {
                    println!("Messaggio non parseabile: {txt}");
                }
            }
        }
    }

    {
        let mut guard = state.lock().unwrap();
        guard.clients.retain(|c| !c.is_closed());
        if guard.clients.is_empty() {
            println!("Nessun utente => mantengo logs e active_users");
        } else {
            println!("Un client e` uscito. Rimasti: {}", guard.clients.len());
        }
    }

    let _ = send_task.await;
    println!("handle_connection terminata");
}

async fn store_and_broadcast(state: State, chat_msg: ChatMessage) {
    // Non serve mut
    let new_list: Vec<ChatMessage>;
    {
        let mut guard = state.lock().unwrap();
        guard.logs.push(chat_msg.clone());

        // 1) Inseriamo l'utente in active_users
        guard.active_users.insert(chat_msg.username.clone());
        // 2) Stampiamo la dimensione
        println!("(DEBUG) Ora active_users ha {} utenti", guard.active_users.len());

        new_list = guard.logs.clone();
    }
    let full_msg = ServerMessage::FullHistory(new_list);
    broadcast_to_all(state, full_msg).await;
}

async fn broadcast_to_all(state: State, smsg: ServerMessage) {
    let text = serde_json::to_string(&smsg).unwrap();
    let warp_msg = Message::text(text);

    let guard = state.lock().unwrap();
    for c in &guard.clients {
        let _ = c.send(warp_msg.clone());
    }
}