//! The chat of a Twitch channel, to read along with its stream.
//!
//! Twitch's chat is IRC, and anyone may read it without an account under a
//! `justinfan` name. It is reached the way Twitch's own page does, as text
//! frames of a WebSocket on port 443: that gets through networks and proxies
//! that let nothing but HTTPS out. A thread of its own keeps the connection
//! and hands what is said to the window; only reading, nothing is ever sent
//! to the chat.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crate::http;

const HOST: &str = "irc-ws.chat.twitch.tv";
/// How long a read waits before the thread looks whether it is still wanted.
const READ_WAIT: Duration = Duration::from_millis(500);
/// Twitch pings every five minutes or so: silence longer than this means
/// the connection is dead.
const SILENCE: Duration = Duration::from_secs(400);
/// Messages kept for showing.
const KEPT: usize = 200;
/// The longest a frame may be; chat lines are far shorter.
const MAX_FRAME: usize = 1 << 20;
/// The colours Twitch gives those who chose none.
const DEFAULT_COLORS: [[u8; 3]; 15] = [
    [0xFF, 0x00, 0x00], [0x00, 0x00, 0xFF], [0x00, 0x80, 0x00], [0xB2, 0x22, 0x22], [0xFF, 0x7F, 0x50],
    [0x9A, 0xCD, 0x32], [0xFF, 0x45, 0x00], [0x2E, 0x8B, 0x57], [0xDA, 0xA5, 0x20], [0xD2, 0x69, 0x1E],
    [0x5F, 0x9E, 0xA0], [0x1E, 0x90, 0xFF], [0xFF, 0x69, 0xB4], [0x8A, 0x2B, 0xE2], [0x00, 0xFF, 0x7F],
];

/// One thing said in the chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Twitch's id of the message, by which a moderator removes it.
    pub id: String,
    /// The author's login, by which a moderator removes all they said.
    pub login: String,
    /// The author's name as they write it.
    pub name: String,
    /// The author's colour, light enough to read on a dark background.
    pub color: [u8; 3],
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Said(Message),
    /// A moderator removed one message.
    Removed(String),
    /// A moderator removed everything a user said (a ban or a timeout).
    Silenced(String),
    /// A moderator cleared the chat.
    Cleared,
    Connected,
    /// The connection failed or was lost; it is tried again by itself.
    Lost(String),
}

/// The connection to a channel's chat. Dropping it closes the connection.
pub struct Chat {
    channel: String,
    events: mpsc::Receiver<Event>,
    stop: Arc<AtomicBool>,
}

impl Chat {
    /// Joins `channel`'s chat. `wake` is called (from another thread) when
    /// there is something new to take with [`events`](Self::events).
    pub fn open(channel: &str, wake: impl Fn() + Send + 'static) -> Self {
        let (tx, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let (name, flag) = (channel.to_owned(), stop.clone());
        std::thread::spawn(move || run(&name, &tx, &flag, &wake));
        Self { channel: channel.to_owned(), events, stop }
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// What happened since the last call.
    pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
        self.events.try_iter()
    }
}

impl Drop for Chat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// What the chat looks like now: the latest messages, less what moderators
/// took out.
#[derive(Default)]
pub struct Log {
    pub messages: VecDeque<Message>,
    pub connected: bool,
    /// Why there is no connection, if it failed.
    pub trouble: Option<String>,
    /// Grows with every change, to notice one cheaply.
    pub serial: u64,
}

impl Log {
    pub fn apply(&mut self, event: Event) {
        self.serial += 1;
        match event {
            Event::Said(message) => {
                self.messages.push_back(message);
                while self.messages.len() > KEPT {
                    self.messages.pop_front();
                }
            }
            Event::Removed(id) => self.messages.retain(|m| m.id != id),
            Event::Silenced(login) => self.messages.retain(|m| m.login != login),
            Event::Cleared => self.messages.clear(),
            Event::Connected => {
                self.connected = true;
                self.trouble = None;
            }
            Event::Lost(why) => {
                self.connected = false;
                self.trouble = Some(why);
            }
        }
    }
}

/// The chat thread: connects, and connects again after a while when the
/// connection is lost, until the `Chat` is dropped.
fn run(channel: &str, tx: &mpsc::Sender<Event>, stop: &AtomicBool, wake: &dyn Fn()) {
    let mut failures = 0u32;
    while !stop.load(Ordering::SeqCst) {
        let mut joined = false;
        let result = session(channel, stop, &mut |event| {
            joined |= event == Event::Connected;
            let sent = tx.send(event).is_ok();
            wake();
            sent
        });
        let Err(why) = result else { return };
        if stop.load(Ordering::SeqCst) || tx.send(Event::Lost(why)).is_err() {
            return;
        }
        wake();
        failures = if joined { 0 } else { failures + 1 };
        // Soon after a connection that worked, later and later otherwise.
        let wait = Duration::from_secs(2 << failures.min(4));
        let since = Instant::now();
        while since.elapsed() < wait {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// One connection, from joining to its loss. `Ok` when it is no longer
/// wanted (`stop`, or `emit` says nobody listens).
fn session(channel: &str, stop: &AtomicBool, emit: &mut dyn FnMut(Event) -> bool) -> Result<(), String> {
    let network = |e: io::Error| format!("нет связи с чатом ({e})");
    let stream = http::tls_connect(HOST, 443, READ_WAIT).map_err(|e| format!("нет связи с чатом ({e})"))?;
    let mut socket = WebSocket::open(stream, HOST, stop).map_err(network)?;
    // An anonymous reader: any number will do for the name.
    let nick = format!("justinfan{}", 10_000 + noise() % 80_000);
    for line in ["CAP REQ :twitch.tv/tags twitch.tv/commands".to_owned(), format!("NICK {nick}"), format!("JOIN #{channel}")] {
        socket.send_text(&line).map_err(network)?;
    }
    let mut heard = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let Some(text) = socket.read_text().map_err(network)? else {
            if heard.elapsed() > SILENCE {
                return Err("чат замолчал".to_owned());
            }
            continue;
        };
        heard = Instant::now();
        for line in text.lines() {
            match parse(line) {
                Some(Incoming::Ping(token)) => socket.send_text(&format!("PONG :{token}")).map_err(network)?,
                Some(Incoming::Reconnect) => return Err("Twitch переключает сервер чата".to_owned()),
                Some(Incoming::Event(event)) => {
                    if !emit(event) {
                        return Ok(());
                    }
                }
                None => {}
            }
        }
    }
}

/// A few bits that differ from run to run; nothing here needs real randomness.
fn noise() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x5EED, |d| d.subsec_nanos() ^ d.as_secs() as u32)
}

// ------------------------------------------------------------------ IRC

#[derive(Debug, PartialEq, Eq)]
enum Incoming {
    Ping(String),
    /// The server is going away and asks to connect anew.
    Reconnect,
    Event(Event),
}

/// Reads one line of Twitch's IRC; `None` for what the player has no use for.
fn parse(line: &str) -> Option<Incoming> {
    let mut rest = line.trim_end();
    let mut tags = "";
    if let Some(tagged) = rest.strip_prefix('@') {
        (tags, rest) = tagged.split_once(' ')?;
    }
    let mut prefix = "";
    if let Some(prefixed) = rest.strip_prefix(':') {
        (prefix, rest) = prefixed.split_once(' ')?;
    }
    let (command, params) = rest.split_once(' ').unwrap_or((rest, ""));
    // The last parameter, after " :", may hold spaces.
    let (middle, trailing) = match params.strip_prefix(':') {
        Some(only) => ("", Some(only)),
        None => match params.split_once(" :") {
            Some((middle, trailing)) => (middle, Some(trailing)),
            None => (params, None),
        },
    };
    let tag = |name: &str| tags.split(';').filter_map(|t| t.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| unescape(v));
    match command {
        "PING" => Some(Incoming::Ping(trailing.unwrap_or(middle).to_owned())),
        "RECONNECT" => Some(Incoming::Reconnect),
        // The end of the server's greeting, and the channel joined.
        "ROOMSTATE" => Some(Incoming::Event(Event::Connected)),
        "PRIVMSG" => {
            let login = prefix.split('!').next().unwrap_or_default().to_owned();
            let text = trailing?;
            // "/me does something" arrives wrapped as a CTCP ACTION.
            let text = text.strip_prefix("\u{1}ACTION ").map_or(text, |t| t.trim_end_matches('\u{1}')).trim();
            if text.is_empty() || login.is_empty() {
                return None;
            }
            let name = tag("display-name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| login.clone());
            let color = tag("color").and_then(|c| hex_color(&c)).unwrap_or_else(|| default_color(&login));
            Some(Incoming::Event(Event::Said(Message {
                id: tag("id").unwrap_or_default(),
                login,
                name,
                color: readable(color),
                text: text.to_owned(),
            })))
        }
        "CLEARMSG" => tag("target-msg-id").map(|id| Incoming::Event(Event::Removed(id))),
        "CLEARCHAT" => Some(Incoming::Event(match trailing {
            Some(login) => Event::Silenced(login.to_owned()),
            None => Event::Cleared,
        })),
        _ => None,
    }
}

/// Undoes the escaping of an IRC tag's value.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some(':') => out.push(';'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn hex_color(text: &str) -> Option<[u8; 3]> {
    let hex = text.strip_prefix('#').filter(|h| h.len() == 6 && h.is_ascii())?;
    let part = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([part(0)?, part(2)?, part(4)?])
}

/// The same colour every time for those who chose none, as Twitch does.
fn default_color(login: &str) -> [u8; 3] {
    let sum = login.bytes().fold(0usize, |sum, b| sum.wrapping_mul(31).wrapping_add(b as usize));
    DEFAULT_COLORS[sum % DEFAULT_COLORS.len()]
}

/// Lightens a colour too dark to read on the player's dark background
/// (pure blue is a favourite), keeping its hue.
fn readable(color: [u8; 3]) -> [u8; 3] {
    const ENOUGH: f32 = 110.0;
    let [r, g, b] = color.map(f32::from);
    let brightness = 0.299 * r + 0.587 * g + 0.114 * b;
    if brightness >= ENOUGH {
        return color;
    }
    // Towards white by as much as is missing.
    let mix = (ENOUGH - brightness) / (255.0 - brightness);
    color.map(|c| (f32::from(c) + (255.0 - f32::from(c)) * mix).round() as u8)
}

// ------------------------------------------------------------ WebSocket

/// The little of a WebSocket client that reading a chat takes: text frames
/// both ways, and answering pings.
struct WebSocket<S> {
    stream: S,
    /// Bytes received and not yet used.
    buffer: Vec<u8>,
    /// A message whose frames are still arriving.
    partial: Vec<u8>,
}

impl<S: Read + Write> WebSocket<S> {
    /// Upgrades a connection just made to `host`.
    fn open(mut stream: S, host: &str, stop: &AtomicBool) -> io::Result<Self> {
        // The key only proves the answer was made for this request, which is
        // of no use here: any valid one does.
        let request = format!(
            "GET / HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: UmVzdW1vIGNoYXQgcmVhZA==\r\nSec-WebSocket-Version: 13\r\n\
             Origin: https://www.twitch.tv\r\n\r\n"
        );
        stream.write_all(request.as_bytes())?;
        stream.flush()?;
        let mut socket = Self { stream, buffer: Vec::new(), partial: Vec::new() };
        let asked = Instant::now();
        loop {
            if let Some(end) = socket.buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&socket.buffer[..end]).into_owned();
                socket.buffer.drain(..end + 4);
                let status = head.lines().next().unwrap_or_default();
                return if status.split(' ').nth(1) == Some("101") {
                    Ok(socket)
                } else {
                    Err(io::Error::other(format!("сервер чата ответил «{status}»")))
                };
            }
            if stop.load(Ordering::SeqCst) || asked.elapsed() > Duration::from_secs(15) || socket.buffer.len() > 16 * 1024 {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "сервер чата не ответил"));
            }
            socket.receive()?;
        }
    }

    /// Reads what has arrived, waiting no longer than the stream's timeout.
    fn receive(&mut self) -> io::Result<()> {
        let mut chunk = [0u8; 4096];
        match self.stream.read(&mut chunk) {
            Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                self.buffer.extend_from_slice(&chunk[..n]);
                Ok(())
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The next text message, or `None` if none arrived within the stream's
    /// timeout.
    fn read_text(&mut self) -> io::Result<Option<String>> {
        let mut waited = false;
        loop {
            while let Some((last, opcode, payload)) = take_frame(&mut self.buffer)? {
                match opcode {
                    // A text, or more of it.
                    0 | 1 => {
                        self.partial.extend_from_slice(&payload);
                        if last {
                            let text = String::from_utf8_lossy(&self.partial).into_owned();
                            self.partial.clear();
                            return Ok(Some(text));
                        }
                    }
                    8 => return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "сервер закрыл соединение")),
                    9 => self.send(10, &payload)?,
                    _ => {}
                }
            }
            if waited {
                return Ok(None);
            }
            self.receive()?;
            waited = true;
        }
    }

    fn send_text(&mut self, text: &str) -> io::Result<()> {
        self.send(1, text.as_bytes())
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        self.stream.write_all(&frame(opcode, payload, noise().to_le_bytes()))?;
        self.stream.flush()
    }
}

/// A frame from the client: whole, and masked as the protocol demands.
fn frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut out = vec![0x80 | opcode];
    match payload.len() {
        n @ 0..=125 => out.push(0x80 | n as u8),
        n @ 126..=0xFFFF => {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().zip(mask.iter().cycle()).map(|(byte, key)| byte ^ key));
    out
}

/// Takes a frame off the front of `buffer` once all of it is there:
/// whether it is its message's last, its opcode, and its payload.
fn take_frame(buffer: &mut Vec<u8>) -> io::Result<Option<(bool, u8, Vec<u8>)>> {
    let [first, second, ..] = buffer[..] else { return Ok(None) };
    let (length, mut header) = match second & 0x7F {
        126 => match buffer.get(2..4) {
            Some(bytes) => (u16::from_be_bytes([bytes[0], bytes[1]]) as usize, 4),
            None => return Ok(None),
        },
        127 => match buffer.get(2..10) {
            Some(bytes) => (u64::from_be_bytes(bytes.try_into().expect("eight bytes")).min(usize::MAX as u64) as usize, 10),
            None => return Ok(None),
        },
        short => (short as usize, 2),
    };
    if length > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "слишком длинное сообщение"));
    }
    // Servers don't mask, but the protocol lets them.
    let mask = if second & 0x80 != 0 {
        let Some(key) = buffer.get(header..header + 4) else { return Ok(None) };
        let key = [key[0], key[1], key[2], key[3]];
        header += 4;
        Some(key)
    } else {
        None
    };
    if buffer.len() < header + length {
        return Ok(None);
    }
    let mut payload: Vec<u8> = buffer[header..header + length].to_vec();
    if let Some(key) = mask {
        payload.iter_mut().zip(key.iter().cycle()).for_each(|(byte, key)| *byte ^= key);
    }
    buffer.drain(..header + length);
    Ok(Some((first & 0x80 != 0, first & 0x0F, payload)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(line: &str) -> Message {
        match parse(line) {
            Some(Incoming::Event(Event::Said(m))) => m,
            other => panic!("not a message: {other:?}"),
        }
    }

    #[test]
    fn chat_lines_are_read() {
        let m = said(
            "@badge-info=;color=#1E90FF;display-name=Вася\\sПупкин;emotes=;id=abc-1;mod=0 \
             :vasya!vasya@vasya.tmi.twitch.tv PRIVMSG #channel :привет: как дела? Kappa",
        );
        assert_eq!((m.id.as_str(), m.login.as_str(), m.name.as_str()), ("abc-1", "vasya", "Вася Пупкин"));
        assert_eq!(m.text, "привет: как дела? Kappa");
        assert_eq!(m.color, [0x1E, 0x90, 0xFF]);

        // No tags at all, and "/me".
        let m = said(":petya!petya@petya.tmi.twitch.tv PRIVMSG #channel :\u{1}ACTION машет рукой\u{1}");
        assert_eq!((m.name.as_str(), m.text.as_str()), ("petya", "машет рукой"));
        assert_eq!(m.color, readable(default_color("petya")));
        // No colour chosen: an empty tag.
        assert_eq!(said("@color=;display-name= :a!a@a PRIVMSG #c :x").name, "a");
    }

    #[test]
    fn service_lines_are_read() {
        assert_eq!(parse("PING :tmi.twitch.tv"), Some(Incoming::Ping("tmi.twitch.tv".into())));
        assert_eq!(parse(":tmi.twitch.tv RECONNECT"), Some(Incoming::Reconnect));
        assert_eq!(
            parse("@emote-only=0;room-id=1 :tmi.twitch.tv ROOMSTATE #channel"),
            Some(Incoming::Event(Event::Connected))
        );
        assert_eq!(
            parse("@login=vasya;target-msg-id=abc-1 :tmi.twitch.tv CLEARMSG #channel :привет"),
            Some(Incoming::Event(Event::Removed("abc-1".into())))
        );
        assert_eq!(
            parse("@ban-duration=600 :tmi.twitch.tv CLEARCHAT #channel :vasya"),
            Some(Incoming::Event(Event::Silenced("vasya".into())))
        );
        assert_eq!(parse(":tmi.twitch.tv CLEARCHAT #channel"), Some(Incoming::Event(Event::Cleared)));
        assert_eq!(parse(":tmi.twitch.tv 001 justinfan1 :Welcome, GLHF!"), None);
        assert_eq!(parse("@msg-id=sub :tmi.twitch.tv USERNOTICE #channel :ура"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn dark_colours_are_lightened() {
        assert_eq!(readable([0xFF, 0x69, 0xB4]), [0xFF, 0x69, 0xB4]);
        let [r, g, b] = readable([0, 0, 0xFF]);
        assert!(b == 0xFF && r == g && r > 80, "{r} {g} {b}");
        let [r, g, b] = readable([0, 0, 0]).map(u32::from);
        assert!(r == g && g == b && r >= 109);
    }

    #[test]
    fn the_log_keeps_the_latest_and_obeys_moderators() {
        let message = |n: usize, login: &str| Message {
            id: format!("id{n}"),
            login: login.into(),
            name: login.into(),
            color: [255; 3],
            text: format!("{n}"),
        };
        let mut log = Log::default();
        for n in 0..KEPT + 5 {
            log.apply(Event::Said(message(n, if n % 2 == 0 { "even" } else { "odd" })));
        }
        assert_eq!(log.messages.len(), KEPT);
        assert_eq!(log.messages[0].text, "5");
        log.apply(Event::Removed("id5".into()));
        assert_eq!(log.messages[0].text, "6");
        log.apply(Event::Silenced("odd".into()));
        assert!(log.messages.iter().all(|m| m.login == "even"));
        log.apply(Event::Lost("нет сети".into()));
        assert!(!log.connected && log.trouble.is_some());
        log.apply(Event::Connected);
        assert!(log.connected && log.trouble.is_none());
        log.apply(Event::Cleared);
        assert!(log.messages.is_empty());
    }

    #[test]
    fn frames_round_trip() {
        for length in [0, 5, 125, 126, 300, 70_000] {
            let payload: Vec<u8> = (0..length).map(|i| i as u8).collect();
            let mut bytes = frame(1, &payload, [1, 2, 3, 4]);
            bytes.extend_from_slice(&[0x89, 0]); // and a ping behind it
            // Nothing until the whole frame is there.
            let mut part = bytes[..bytes.len() - 3].to_vec();
            assert_eq!(take_frame(&mut part).unwrap(), None);
            assert_eq!(take_frame(&mut bytes).unwrap(), Some((true, 1, payload)));
            assert_eq!(take_frame(&mut bytes).unwrap(), Some((true, 9, vec![])));
            assert!(bytes.is_empty());
        }
        let mut huge = vec![0x81, 127, 0, 0, 0, 1, 0, 0, 0, 0];
        assert!(take_frame(&mut huge).is_err());
    }

    /// A server's side of a conversation, played back to the client.
    struct Script {
        incoming: io::Cursor<Vec<u8>>,
        sent: Vec<u8>,
    }

    impl Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.incoming.read(buf)? {
                0 => Err(io::ErrorKind::WouldBlock.into()),
                n => Ok(n),
            }
        }
    }

    impl Write for Script {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.sent.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_websocket_conversation() {
        // Unmasked frames, as servers send them.
        let server = |opcode: u8, last: bool, payload: &[u8]| {
            let mut f = vec![if last { 0x80 } else { 0 } | opcode, payload.len() as u8];
            f.extend_from_slice(payload);
            f
        };
        let mut incoming = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_vec();
        incoming.extend(server(1, true, b"one\r\ntwo\r\n"));
        incoming.extend(server(9, true, b"hi"));
        incoming.extend(server(1, false, "пол".as_bytes()));
        incoming.extend(server(0, true, "овина".as_bytes()));
        let stop = AtomicBool::new(false);
        let script = Script { incoming: io::Cursor::new(incoming), sent: Vec::new() };
        let mut socket = WebSocket::open(script, "example.org", &stop).unwrap();
        assert!(socket.stream.sent.starts_with(b"GET / HTTP/1.1\r\nHost: example.org\r\n"));
        socket.stream.sent.clear();

        assert_eq!(socket.read_text().unwrap().as_deref(), Some("one\r\ntwo\r\n"));
        assert_eq!(socket.read_text().unwrap().as_deref(), Some("половина"));
        assert_eq!(socket.read_text().unwrap(), None);
        // The ping was answered with a pong carrying the same bytes.
        let mut sent = socket.stream.sent.clone();
        assert_eq!(take_frame(&mut sent).unwrap(), Some((true, 10, b"hi".to_vec())));

        let refused = Script { incoming: io::Cursor::new(b"HTTP/1.1 403 Forbidden\r\n\r\n".to_vec()), sent: Vec::new() };
        assert!(WebSocket::open(refused, "example.org", &stop).is_err());
    }

    /// `RESUMO_TEST_TWITCH=channel cargo test -- --ignored reads_a_live_chat --nocapture`
    #[test]
    #[ignore = "needs the network and a busy channel"]
    fn reads_a_live_chat() {
        let channel = std::env::var("RESUMO_TEST_TWITCH").expect("RESUMO_TEST_TWITCH");
        let chat = Chat::open(&channel, || {});
        let mut log = Log::default();
        let since = Instant::now();
        while since.elapsed() < Duration::from_secs(20) && log.messages.len() < 3 {
            for event in chat.events() {
                println!("{event:?}");
                log.apply(event);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(log.connected, "{:?}", log.trouble);
    }
}
