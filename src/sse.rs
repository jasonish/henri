// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use futures::Stream;
use futures::StreamExt;

pub(crate) struct SseStream<S> {
    stream: S,
    line_buffer: String,
}

impl<S> SseStream<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            stream,
            line_buffer: String::new(),
        }
    }
}

impl<S, B, E> SseStream<S>
where
    S: Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    pub(crate) async fn next_event(&mut self) -> Option<Result<String, E>> {
        loop {
            while let Some(newline_pos) = self.line_buffer.find('\n') {
                let line = self.line_buffer[..newline_pos].to_string();
                self.line_buffer = self.line_buffer[newline_pos + 1..].to_string();

                let data = match line.strip_prefix("data: ") {
                    Some(d) => d,
                    None => match line.strip_prefix("data:") {
                        Some(d) => d.trim(),
                        None => continue,
                    },
                };

                if data == "[DONE]" {
                    continue;
                }

                return Some(Ok(data.to_string()));
            }

            match self.stream.next().await {
                Some(Ok(chunk)) => {
                    let chunk_str = String::from_utf8_lossy(chunk.as_ref());
                    self.line_buffer.push_str(&chunk_str);
                }
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }
    }
}
