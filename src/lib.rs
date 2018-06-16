extern crate url;

use std::io::prelude::*;
use std::io::BufReader;
use std::thread;
use std::sync::Arc;
use std::sync::Mutex;
use std::collections::HashMap;
use url::{Url, ParseError};
use std::net::{Shutdown, TcpStream};

mod network;


pub struct EventSource {
    ready_state: Arc<Mutex<State>>,
    listeners: Arc<Mutex<HashMap<String, Vec<Box<Fn(Event) + Send>>>>>,
    on_open_listeners: Arc<Mutex<Vec<Box<Fn() + Send>>>>,
    stream: TcpStream
}

#[derive(Debug, Clone)]
pub struct Event {
    type_: String,
    data: String
}

#[derive(Debug, PartialEq, Clone)]
pub enum State {
    CONNECTING,
    OPEN,
    CLOSED
}

impl EventSource {
    pub fn new(url: &str) -> Result<EventSource, ParseError> {
        let stream = network::open_connection(Url::parse(url)?).unwrap();

        let listeners = Arc::new(Mutex::new(HashMap::new()));
        let on_open_listeners = Arc::new(Mutex::new(vec!()));
        let ready_state = Arc::new(Mutex::new(State::CONNECTING));

        listen_to_stream(
            stream.try_clone().unwrap(),
            Arc::clone(&ready_state),
            Arc::clone(&listeners),
            Arc::clone(&on_open_listeners)
        );

        Ok(EventSource{ ready_state, listeners, stream: stream, on_open_listeners })
    }

    pub fn close(&self) {
        self.stream.shutdown(Shutdown::Both).unwrap();
        let mut state = self.ready_state.lock().unwrap();
        *state = State::CLOSED;
    }

    pub fn on_open<F>(&self, listener: F) where F: Fn() + Send + 'static {
        let mut listeners = self.on_open_listeners.lock().unwrap();
        listeners.push(Box::new(listener));
    }

    pub fn on_message<F>(&self, listener: F) where F: Fn(Event) + Send + 'static {
        self.add_event_listener("message", listener);
    }

    pub fn add_event_listener<F>(&self, event_type: &str, listener: F) where F: Fn(Event) + Send + 'static {
        let mut listeners = self.listeners.lock().unwrap();
        let listener = Box::new(listener);

        if listeners.contains_key(event_type) {
            listeners.get_mut(event_type).unwrap().push(listener);
        } else {
            listeners.insert(String::from(event_type), vec!(listener));
        }
    }

    pub fn state(&self) -> State {
        let state = &self.ready_state.lock().unwrap();
        (*state).clone()
    }
}

fn listen_to_stream(
    stream: TcpStream,
    state: Arc<Mutex<State>>,
    listeners: Arc<Mutex<HashMap<String, Vec<Box<Fn(Event) + Send>>>>>,
    on_open_listeners: Arc<Mutex<Vec<Box<Fn() + Send>>>>
) {
    thread::spawn(move || {
        let reader = BufReader::new(stream.try_clone().unwrap());
        let mut pending_event: Option<Event> = None;

        for line in reader.lines() {
            let line = line.unwrap();
            let mut state = state.lock().unwrap();

            match *state {
                State::CONNECTING => *state = handle_stream_header(line, &on_open_listeners),
                _ => pending_event = handle_stream_body(pending_event, line, &listeners)
            }
        }
    });
}

fn handle_stream_header(line: String, listeners: &Arc<Mutex<Vec<Box<Fn() + Send>>>>) -> State {
    if line == "" {
        dispatch_open_event(listeners);
        State::OPEN
    } else {
        State::CONNECTING
    }
}

fn handle_stream_body(
    pending_event: Option<Event>,
    line: String,
    listeners: &Arc<Mutex<HashMap<String, Vec<Box<Fn(Event) + Send>>>>>
) -> Option<Event> {
    let mut event = None;

    if line == "" {
        if let Some(e) = pending_event {
            dispatch_event(listeners, &e);
        }
    } else if !line.starts_with(":") {
        event = update_event(pending_event, line);
    }

    event
}

fn dispatch_event(listeners: &Arc<Mutex<HashMap<String, Vec<Box<Fn(Event) + Send>>>>>, event: &Event) {
    let listeners = listeners.lock().unwrap();
    if listeners.contains_key(&event.type_) {
        for listener in listeners.get(&event.type_).unwrap().iter() {
            listener(event.clone())
        }
    }
}

fn dispatch_open_event(listeners: &Arc<Mutex<Vec<Box<Fn() + Send>>>>) {
    let listeners = listeners.lock().unwrap();
    for listener in listeners.iter() {
        listener()
    }
}

fn update_event(pending_event: Option<Event>, message: String) -> Option<Event> {
    let mut event = match pending_event {
        Some(e) => e.clone(),
        None => Event { type_: String::from("message"), data: String::from("") }
    };

    match parse_field(&message) {
        ("event", value) => event.type_ = String::from(value),
        ("data", value) => event.data = String::from(value),
        _ => ()
    }

    Some(event)
}

fn parse_field<'a>(message: &'a String) -> (&'a str, &'a str) {
    let parts: Vec<&str> = message.split(":").collect();
    (parts[0], parts[1].trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    mod fake_server;

    fn setup() -> (EventSource, fake_server::FakeServer) {
        let fake_server = fake_server::FakeServer::new();
        let address = format!("http://{}/sub", fake_server.socket_address());
        let event_source = EventSource::new(address.as_str()).unwrap();

        (event_source, fake_server)
    }

    #[test]
    fn should_create_client() {
        let (event_source, fake_server) = setup();
        event_source.close();
        fake_server.close();
    }

    #[test]
    fn should_thrown_an_error_when_malformed_url_provided() {
        match EventSource::new("127.0.0.1:1236/sub") {
            Ok(_) => assert!(false, "should had thrown an error"),
            Err(_) => assert!(true)
        }
    }

    #[test]
    fn accept_closure_as_listeners() {
        static mut CALL_COUNT: i32 = 0;
        static mut IS_RIGHT_MESSAGE: bool = false;

        let (event_source, fake_server) = setup();

        event_source.on_message(move |message| {
            unsafe {
                CALL_COUNT += 1;
                IS_RIGHT_MESSAGE = message.data == "some message";
            }
        });

        fake_server.send("\ndata: some message\n\n");

        unsafe {
            let mut retry_count = 0;
            while CALL_COUNT == 0 && retry_count < 5 {
              thread::sleep(Duration::from_millis(100));
              retry_count += 1;
            }

            assert_eq!(CALL_COUNT, 1);
            assert!(IS_RIGHT_MESSAGE);
        }

        event_source.close();
        fake_server.close();
    }

    #[test]
    fn should_trigger_listeners_when_message_received() {
        static mut CALL_COUNT: i32 = 0;
        static mut IS_RIGHT_MESSAGE: bool = false;

        let (event_source, fake_server) = setup();

        event_source.on_message(|message| {
            unsafe {
                CALL_COUNT += 1;
                IS_RIGHT_MESSAGE = message.data == "some message";
            }
        });

        fake_server.send("\ndata: some message\n\n");

        unsafe {
            let mut retry_count = 0;
            while CALL_COUNT == 0 && retry_count < 5 {
              thread::sleep(Duration::from_millis(100));
              retry_count += 1;
            }

            assert_eq!(CALL_COUNT, 1);
            assert!(IS_RIGHT_MESSAGE);
        }

        event_source.close();
        fake_server.close();
    }

    #[test]
    fn should_not_trigger_listeners_for_comments() {
        static mut CALL_COUNT: i32 = 0;

        let (event_source, fake_server) = setup();

        event_source.on_message(|_| {
            unsafe {
                CALL_COUNT += 1;
            }
        });

        fake_server.send("\n");
        fake_server.send("data: message\n\n");
        fake_server.send(":this is a comment\n");
        fake_server.send(":this is another comment\n");
        fake_server.send("data: this is a message\n\n");

        unsafe {
            thread::sleep(Duration::from_millis(500));
            assert_eq!(CALL_COUNT, 2);
        }

        event_source.close();
        fake_server.close();
    }

    #[test]
    fn ensure_stream_is_parsed_after_headers() {
        static mut CALL_COUNT: i32 = 0;

        let (event_source, fake_server) = setup();

        event_source.on_message(|_| {
            unsafe {
                CALL_COUNT += 1;
            }
        });

        fake_server.send("HTTP/1.1 200 OK\n");
        fake_server.send("Server: nginx/1.10.3\n");
        fake_server.send("Date: Thu, 24 May 2018 12:26:38 GMT\n");
        fake_server.send("Content-Type: text/event-stream; charset=utf-8\n");
        fake_server.send("Connection: keep-alive\n");
        fake_server.send("\n");
        fake_server.send("data: this is a message\n\n");

        unsafe {
            thread::sleep(Duration::from_millis(300));
            assert_eq!(CALL_COUNT, 1);
        }

        event_source.close();
        fake_server.close();
    }

    #[test]
    fn ignore_empty_messages() {
        static mut CALL_COUNT: i32 = 0;

        let (event_source, fake_server) = setup();

        event_source.on_message(|_| {
            unsafe {
                CALL_COUNT += 1;
            }
        });

        fake_server.send("\n");
        fake_server.send("data: message\n");
        fake_server.send("\n");
        fake_server.send("\n");
        fake_server.send("data: this is a message\n\n");

        unsafe {
            thread::sleep(Duration::from_millis(500));
            assert_eq!(CALL_COUNT, 2);
        }

        event_source.close();
        fake_server.close();
    }

    #[test]
    fn event_trigger_its_defined_listener() {
        static mut IS_RIGHT_EVENT: bool = false;

        let (event_source, fake_server) = setup();

        event_source.add_event_listener("myEvent", |event| {
            unsafe {
                IS_RIGHT_EVENT = event.type_ == String::from("myEvent");
                IS_RIGHT_EVENT = IS_RIGHT_EVENT && event.data == String::from("my message");
            }
        });

        fake_server.send("\n");
        fake_server.send("event: myEvent\n");
        fake_server.send("data: my message\n\n");

        unsafe {
            thread::sleep(Duration::from_millis(500));
            assert!(IS_RIGHT_EVENT);
        }

        event_source.close();
        fake_server.close();
    }

    #[test]
    fn dont_trigger_on_message_for_event() {
        static mut ON_MESSAGE_WAS_CALLED: bool = false;

        let (event_source, fake_server) = setup();

        event_source.on_message(|_| {
            unsafe {
                ON_MESSAGE_WAS_CALLED = true;
            }
        });

        fake_server.send("\n");
        fake_server.send("event: myEvent\n");
        fake_server.send("data: my message\n\n");

        unsafe {
            thread::sleep(Duration::from_millis(500));
            assert!(!ON_MESSAGE_WAS_CALLED);
        }

        event_source.close();
        fake_server.close();
    }

    #[test]
    fn should_close_connection() {
        static mut CALL_COUNT: i32 = 0;

        let (event_source, fake_server) = setup();

        event_source.on_message(|_| {
            unsafe {
                CALL_COUNT += 1;
            }
        });

        fake_server.send("\ndata: some message\n\n");
        thread::sleep(Duration::from_millis(200));
        event_source.close();
        fake_server.send("\ndata: some message\n\n");

        unsafe {
            thread::sleep(Duration::from_millis(400));

            assert_eq!(CALL_COUNT, 1);
        }

        fake_server.close();
    }

    #[test]
    fn should_trigger_on_open_callback_when_connected() {
        static mut CONNECTION_OPEN: bool = false;
        static mut OPEN_CALLBACK_CALLS: i32 = 0;

        let (event_source, fake_server) = setup();

        event_source.on_open(|| {
            unsafe {
                CONNECTION_OPEN = true;
                OPEN_CALLBACK_CALLS += 1;
            }
        });

        fake_server.send("HTTP/1.1 200 OK\n");
        fake_server.send("Date: Thu, 24 May 2018 12:26:38 GMT\n");
        fake_server.send("\n");
        unsafe {
            thread::sleep(Duration::from_millis(200));
            assert!(CONNECTION_OPEN);
            assert_eq!(OPEN_CALLBACK_CALLS, 1);
        }
        event_source.close();
        fake_server.close();
    }

    #[test]
    fn should_have_status_connecting_while_opening_connection() {
        let (event_source, fake_server) = setup();

        let state = event_source.state();
        assert_eq!(state, State::CONNECTING);

        event_source.close();
        fake_server.close();
    }

    #[test]
    fn should_have_status_open_after_connection_stabilished() {
        let (event_source, fake_server) = setup();

        fake_server.send("\n");
        thread::sleep(Duration::from_millis(200));

        let state = event_source.state();
        assert_eq!(state, State::OPEN);

        event_source.close();
        fake_server.close();
    }

    #[test]
    fn should_have_status_closed_after_closing_connection() {
        let (event_source, fake_server) = setup();

        event_source.close();
        thread::sleep(Duration::from_millis(200));

        let state = event_source.state();
        assert_eq!(state, State::CLOSED);

        fake_server.close();
    }
}
