//! Fake Redis connections for tests that need a Redis that misbehaves.

use redis::aio::ConnectionLike;
use redis::{Cmd, Pipeline, RedisFuture, Value};

/// A connection whose commands never get an answer.
#[derive(Clone)]
pub(crate) struct NeverAnswers;

impl ConnectionLike for NeverAnswers {
    fn req_packed_command<'a>(&'a mut self, _cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        Box::pin(std::future::pending())
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        _pipeline: &'a Pipeline,
        _offset: usize,
        _count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        Box::pin(std::future::pending())
    }

    fn get_db(&self) -> i64 {
        0
    }
}

/// A connection whose commands all fail, like a refused socket.
#[derive(Clone)]
pub(crate) struct Refused;

impl ConnectionLike for Refused {
    fn req_packed_command<'a>(&'a mut self, _cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        Box::pin(std::future::ready(Err(refused())))
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        _pipeline: &'a Pipeline,
        _offset: usize,
        _count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        Box::pin(std::future::ready(Err(refused())))
    }

    fn get_db(&self) -> i64 {
        0
    }
}

fn refused() -> redis::RedisError {
    std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused").into()
}
