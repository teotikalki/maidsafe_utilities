// Copyright 2016 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under (1) the MaidSafe.net Commercial License,
// version 1.0 or later, or (2) The General Public License (GPL), version 3, depending on which
// licence you accepted on initial access to the Software (the "Licences").
//
// By contributing code to the SAFE Network Software, or to this project generally, you agree to be
// bound by the terms of the MaidSafe Contributor Agreement, version 1.0.  This, along with the
// Licenses can be found in the root directory of this project at LICENSE, COPYING and CONTRIBUTOR.
//
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.
//
// Please review the Licences for the specific language governing permissions and limitations
// relating to use of the SAFE Network Software.

// TODO: consider contributing this code to the log4rs crate.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io::{self, Stdout, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;

use log::web_socket::WebSocket;
use log4rs::append::Append;
use log4rs::encode::Encode;
use log4rs::encode::pattern::PatternEncoder;
use log4rs::encode::writer::SimpleWriter;
use log4rs::file::{Deserialize, Deserializers};
use logger::LogRecord;
use regex::Regex;
use serde_value::Value;
use thread::Joiner;

/// Message terminator for streaming to Log Servers. Servers must look out for this sequence which
/// demarcates the end of a particular log message.
pub const MSG_TERMINATOR: [u8; 3] = [254, 253, 255];

pub struct AsyncConsoleAppender;

impl AsyncConsoleAppender {
    pub fn builder() -> AsyncConsoleAppenderBuilder {
        AsyncConsoleAppenderBuilder { encoder: Box::new(PatternEncoder::default()) }
    }
}

pub struct AsyncConsoleAppenderBuilder {
    encoder: Box<Encode>,
}

impl AsyncConsoleAppenderBuilder {
    pub fn encoder(self, encoder: Box<Encode>) -> Self {
        AsyncConsoleAppenderBuilder { encoder: encoder }
    }

    pub fn build(self) -> AsyncAppender {
        AsyncAppender::new(io::stdout(), self.encoder)
    }
}

pub struct AsyncFileAppender;

impl AsyncFileAppender {
    pub fn builder<P: AsRef<Path>>(path: P) -> AsyncFileAppenderBuilder {
        AsyncFileAppenderBuilder {
            path: path.as_ref().to_path_buf(),
            encoder: Box::new(PatternEncoder::default()),
            append: true,
        }
    }
}

pub struct AsyncFileAppenderBuilder {
    path: PathBuf,
    encoder: Box<Encode>,
    append: bool,
}

impl AsyncFileAppenderBuilder {
    pub fn encoder(self, encoder: Box<Encode>) -> Self {
        AsyncFileAppenderBuilder {
            path: self.path,
            encoder: encoder,
            append: self.append,
        }
    }

    pub fn append(self, append: bool) -> Self {
        AsyncFileAppenderBuilder {
            path: self.path,
            encoder: self.encoder,
            append: append,
        }
    }

    pub fn build(self) -> io::Result<AsyncAppender> {
        let file = try!(OpenOptions::new()
            .write(true)
            .append(self.append)
            .create(true)
            .open(self.path));

        Ok(AsyncAppender::new(file, self.encoder))
    }
}

pub struct AsyncServerAppender;

impl AsyncServerAppender {
    pub fn builder<A: ToSocketAddrs>(server_addr: A) -> AsyncServerAppenderBuilder<A> {
        AsyncServerAppenderBuilder {
            addr: server_addr,
            encoder: Box::new(PatternEncoder::default()),
            no_delay: true,
        }
    }
}

pub struct AsyncServerAppenderBuilder<A> {
    addr: A,
    encoder: Box<Encode>,
    no_delay: bool,
}

impl<A: ToSocketAddrs> AsyncServerAppenderBuilder<A> {
    pub fn encoder(self, encoder: Box<Encode>) -> Self {
        AsyncServerAppenderBuilder {
            addr: self.addr,
            encoder: encoder,
            no_delay: self.no_delay,
        }
    }

    pub fn no_delay(self, no_delay: bool) -> Self {
        AsyncServerAppenderBuilder {
            addr: self.addr,
            encoder: self.encoder,
            no_delay: no_delay,
        }
    }

    pub fn build(self) -> io::Result<AsyncAppender> {
        let stream = try!(TcpStream::connect(self.addr));
        try!(stream.set_nodelay(self.no_delay));
        Ok(AsyncAppender::new(stream, self.encoder))
    }
}

pub struct AsyncWebSockAppender;

impl AsyncWebSockAppender {
    pub fn builder<U: Borrow<str>>(server_url: U) -> AsyncWebSockAppenderBuilder<U> {
        AsyncWebSockAppenderBuilder {
            url: server_url,
            encoder: Box::new(PatternEncoder::default()),
        }
    }
}

pub struct AsyncWebSockAppenderBuilder<U> {
    url: U,
    encoder: Box<Encode>,
}

impl<U: Borrow<str>> AsyncWebSockAppenderBuilder<U> {
    pub fn encoder(self, encoder: Box<Encode>) -> Self {
        AsyncWebSockAppenderBuilder {
            url: self.url,
            encoder: encoder,
        }
    }

    pub fn build(self) -> io::Result<AsyncAppender> {
        let ws = try!(WebSocket::new(self.url));
        Ok(AsyncAppender::new(ws, self.encoder))
    }
}

pub struct AsyncConsoleAppenderCreator;

impl Deserialize for AsyncConsoleAppenderCreator {
    type Trait = Append;

    fn deserialize(&self,
                   config: Value,
                   _deserializers: &Deserializers)
                   -> Result<Box<Append>, Box<Error>> {
        let mut map = match config {
            Value::Map(map) => map,
            _ => return Err(Box::new(ConfigError("config must be a map".to_owned()))),
        };

        let pattern = try!(parse_pattern(&mut map, false));
        Ok(Box::new(AsyncConsoleAppender::builder().encoder(Box::new(pattern)).build()))
    }
}

pub struct AsyncFileAppenderCreator;

impl Deserialize for AsyncFileAppenderCreator {
    type Trait = Append;

    fn deserialize(&self,
                   config: Value,
                   _deserializers: &Deserializers)
                   -> Result<Box<Append>, Box<Error>> {
        let mut map = match config {
            Value::Map(map) => map,
            _ => return Err(Box::new(ConfigError("config must be a map".to_owned()))),
        };

        let path = match map.remove(&Value::String("path".to_owned())) {
            Some(Value::String(path)) => path,
            Some(_) => return Err(Box::new(ConfigError("`path` must be a string".to_owned()))),
            None => return Err(Box::new(ConfigError("`path` is required".to_owned()))),
        };

        let append = match map.remove(&Value::String("append".to_owned())) {
            Some(Value::Bool(append)) => append,
            Some(_) => return Err(Box::new(ConfigError("`append` must be a bool".to_owned()))),
            None => true,
        };

        let pattern = try!(parse_pattern(&mut map, false));
        let appender = try!(AsyncFileAppender::builder(path)
            .encoder(Box::new(pattern))
            .append(append)
            .build());

        Ok(Box::new(appender))
    }
}

pub struct AsyncServerAppenderCreator;

impl Deserialize for AsyncServerAppenderCreator {
    type Trait = Append;

    fn deserialize(&self,
                   config: Value,
                   _deserializers: &Deserializers)
                   -> Result<Box<Append>, Box<Error>> {
        let mut map = match config {
            Value::Map(map) => map,
            _ => return Err(Box::new(ConfigError("config must be a map".to_owned()))),
        };

        let server_addr = match map.remove(&Value::String("server_addr".to_owned())) {
            Some(Value::String(addr)) => try!(SocketAddr::from_str(&addr[..])),
            Some(_) => {
                return Err(Box::new(ConfigError("`server_addr` must be a string".to_owned())))
            }
            None => return Err(Box::new(ConfigError("`server_addr` is required".to_owned()))),
        };
        let no_delay = match map.remove(&Value::String("no_delay".to_owned())) {
            Some(Value::Bool(no_delay)) => no_delay,
            Some(_) => return Err(Box::new(ConfigError("`no_delay` must be a boolean".to_owned()))),
            None => true,
        };
        let pattern = try!(parse_pattern(&mut map, false));

        Ok(Box::new(try!(AsyncServerAppender::builder(server_addr)
            .encoder(Box::new(pattern))
            .no_delay(no_delay)
            .build())))
    }
}

pub struct AsyncWebSockAppenderCreator;

impl Deserialize for AsyncWebSockAppenderCreator {
    type Trait = Append;

    fn deserialize(&self,
                   config: Value,
                   _deserializers: &Deserializers)
                   -> Result<Box<Append>, Box<Error>> {
        let mut map = match config {
            Value::Map(map) => map,
            _ => return Err(Box::new(ConfigError("config must be a map".to_owned()))),
        };

        let server_url = match map.remove(&Value::String("server_url".to_owned())) {
            Some(Value::String(url)) => url,
            Some(_) => {
                return Err(Box::new(ConfigError("`server_url` must be a string".to_owned())))
            }
            None => return Err(Box::new(ConfigError("`server_url` is required".to_owned()))),
        };

        let pattern = try!(parse_pattern(&mut map, true));
        Ok(Box::new(try!(AsyncWebSockAppender::builder(server_url)
            .encoder(Box::new(pattern))
            .build())))
    }
}

fn parse_pattern(map: &mut BTreeMap<Value, Value>,
                 is_websocket: bool)
                 -> Result<PatternEncoder, Box<Error>> {
    use rand;

    match map.remove(&Value::String("pattern".to_owned())) {
        Some(Value::String(pattern)) => Ok(PatternEncoder::new(&pattern)),
        Some(_) => Err(Box::new(ConfigError("`pattern` must be a string".to_owned()))),
        None => {
            if is_websocket {
                Ok(make_json_pattern(rand::random()))
            } else {
                Ok(PatternEncoder::default())
            }
        }
    }
}

pub fn make_json_pattern(unique_id: u64) -> PatternEncoder {
    let pattern = format!("{{{{\"id\":\"{}\",\"level\":\"{{l}}\",\"time\":\"{{d}}\",\"thread\":\
                           \"{{T}}\",\"module\":\"{{M}}\",\"file\":\"{{f}}\",\"line\":\"{{L}}\",\
                           \"msg\":\"{{m}}\"}}}}",
                          unique_id);

    PatternEncoder::new(&pattern)
}

#[derive(Debug)]
struct ConfigError(String);

impl Error for ConfigError {
    fn description(&self) -> &str {
        &self.0
    }
}

impl Display for ConfigError {
    fn fmt(&self, fmt: &mut Formatter) -> fmt::Result {
        fmt.write_str(&self.0)
    }
}

enum AsyncEvent {
    Log(Vec<u8>),
    Terminate,
}

#[derive(Debug)]
pub struct AsyncAppender {
    encoder: Box<Encode>,
    tx: Mutex<Sender<AsyncEvent>>,
    _raii_joiner: Joiner,
}

impl AsyncAppender {
    fn new<W: 'static + SyncWrite + Send>(mut writer: W, encoder: Box<Encode>) -> Self {
        let (tx, rx) = mpsc::channel::<AsyncEvent>();

        let joiner = thread!("AsyncLog", move || {
            let re = unwrap!(Regex::new(r"#FS#?.*[/\\#]([^#]+)#FE#"));

            for event in rx.iter() {
                match event {
                    AsyncEvent::Log(mut msg) => {
                        if let Ok(mut str_msg) = String::from_utf8(msg) {
                            let str_msg_cloned = str_msg.clone();
                            if let Some(file_name_capture) = re.captures(&str_msg_cloned) {
                                if let Some(file_name) = file_name_capture.at(1) {
                                    str_msg = re.replace(&str_msg[..], file_name);
                                }
                            }

                            msg = str_msg.into_bytes();
                            let _ = writer.sync_write(&msg);
                        }
                    }
                    AsyncEvent::Terminate => break,
                }
            }
        });

        AsyncAppender {
            encoder: encoder,
            tx: Mutex::new(tx),
            _raii_joiner: Joiner::new(joiner),
        }
    }
}

impl Append for AsyncAppender {
    fn append(&self, record: &LogRecord) -> Result<(), Box<Error>> {
        let mut msg = Vec::new();
        try!(self.encoder.encode(&mut SimpleWriter(&mut msg), record));
        try!(unwrap!(self.tx.lock()).send(AsyncEvent::Log(msg)));
        Ok(())
    }
}

impl Drop for AsyncAppender {
    fn drop(&mut self) {
        let _ = unwrap!(self.tx.lock()).send(AsyncEvent::Terminate);
    }
}

trait SyncWrite {
    fn sync_write(&mut self, buf: &[u8]) -> io::Result<()>;
}

impl SyncWrite for Stdout {
    fn sync_write(&mut self, buf: &[u8]) -> io::Result<()> {
        let mut out = self.lock();
        try!(out.write_all(buf));
        out.flush()
    }
}

impl SyncWrite for File {
    fn sync_write(&mut self, buf: &[u8]) -> io::Result<()> {
        try!(self.write_all(buf));
        self.flush()
    }
}

impl SyncWrite for TcpStream {
    fn sync_write(&mut self, buf: &[u8]) -> io::Result<()> {
        try!(self.write_all(&buf));
        self.write_all(&MSG_TERMINATOR[..])
    }
}

impl SyncWrite for WebSocket {
    fn sync_write(&mut self, buf: &[u8]) -> io::Result<()> {
        self.write_all(buf)
    }
}
