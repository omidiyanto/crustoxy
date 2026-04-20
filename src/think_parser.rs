pub enum ContentType {
    Text,
    Thinking,
}

pub struct ContentChunk {
    pub content_type: ContentType,
    pub content: String,
}

pub struct ThinkTagParser {
    buffer: String,
    current_close_tag: Option<&'static str>,
}

const TAG_PAIRS: &[(&str, &str)] = &[("<think>", "</think>"), ("<thought>", "</thought>")];

impl ThinkTagParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            current_close_tag: None,
        }
    }

    pub fn feed(&mut self, content: &str) -> Vec<ContentChunk> {
        self.buffer.push_str(content);
        let mut chunks = Vec::new();

        loop {
            let prev_len = self.buffer.len();
            let chunk = if self.current_close_tag.is_none() {
                self.parse_outside()
            } else {
                self.parse_inside()
            };

            if let Some(c) = chunk {
                chunks.push(c);
            } else if self.buffer.len() == prev_len {
                break;
            }
        }
        chunks
    }

    fn parse_outside(&mut self) -> Option<ContentChunk> {
        let mut best_open: Option<(&'static str, &'static str, usize)> = None;
        for &(open, close) in TAG_PAIRS {
            if let Some(pos) = self.buffer.find(open) {
                if best_open.is_none() || pos < best_open.unwrap().2 {
                    best_open = Some((open, close, pos));
                }
            }
        }

        let mut best_close: Option<(usize, usize)> = None; // (pos, len)
        for &(_, close) in TAG_PAIRS {
            if let Some(pos) = self.buffer.find(close) {
                if best_close.is_none() || pos < best_close.unwrap().0 {
                    best_close = Some((pos, close.len()));
                }
            }
        }

        if let Some((oc_pos, oc_len)) = best_close {
            if best_open.is_none() || oc_pos < best_open.unwrap().2 {
                let pre = self.buffer[..oc_pos].to_string();
                self.buffer = self.buffer[oc_pos + oc_len..].to_string();
                if !pre.is_empty() {
                    return Some(ContentChunk {
                        content_type: ContentType::Text,
                        content: pre,
                    });
                }
                return None;
            }
        }

        match best_open {
            None => {
                if let Some(last_bracket) = self.buffer.rfind('<') {
                    let potential = &self.buffer[last_bracket..];
                    let plen = potential.len();

                    let mut partial_match = false;
                    for &(open, close) in TAG_PAIRS {
                        if (plen < open.len() && open.starts_with(potential))
                            || (plen < close.len() && close.starts_with(potential))
                        {
                            partial_match = true;
                            break;
                        }
                    }

                    if partial_match {
                        let emit = self.buffer[..last_bracket].to_string();
                        self.buffer = self.buffer[last_bracket..].to_string();
                        if !emit.is_empty() {
                            return Some(ContentChunk {
                                content_type: ContentType::Text,
                                content: emit,
                            });
                        }
                        return None;
                    }
                }
                let emit = std::mem::take(&mut self.buffer);
                if !emit.is_empty() {
                    Some(ContentChunk {
                        content_type: ContentType::Text,
                        content: emit,
                    })
                } else {
                    None
                }
            }
            Some((open_tag, close_tag, pos)) => {
                let pre = self.buffer[..pos].to_string();
                self.buffer = self.buffer[pos + open_tag.len()..].to_string();
                self.current_close_tag = Some(close_tag);
                if !pre.is_empty() {
                    Some(ContentChunk {
                        content_type: ContentType::Text,
                        content: pre,
                    })
                } else {
                    None
                }
            }
        }
    }

    fn parse_inside(&mut self) -> Option<ContentChunk> {
        let close_tag = self.current_close_tag.unwrap();
        match self.buffer.find(close_tag) {
            None => {
                if let Some(last_bracket) = self.buffer.rfind('<') {
                    let remaining = self.buffer.len() - last_bracket;
                    if remaining < close_tag.len() {
                        let potential = &self.buffer[last_bracket..];
                        if close_tag.starts_with(potential) {
                            let emit = self.buffer[..last_bracket].to_string();
                            self.buffer = self.buffer[last_bracket..].to_string();
                            if !emit.is_empty() {
                                return Some(ContentChunk {
                                    content_type: ContentType::Thinking,
                                    content: emit,
                                });
                            }
                            return None;
                        }
                    }
                }
                let emit = std::mem::take(&mut self.buffer);
                if !emit.is_empty() {
                    Some(ContentChunk {
                        content_type: ContentType::Thinking,
                        content: emit,
                    })
                } else {
                    None
                }
            }
            Some(pos) => {
                let thinking = self.buffer[..pos].to_string();
                self.buffer = self.buffer[pos + close_tag.len()..].to_string();
                self.current_close_tag = None;
                if !thinking.is_empty() {
                    Some(ContentChunk {
                        content_type: ContentType::Thinking,
                        content: thinking,
                    })
                } else {
                    None
                }
            }
        }
    }

    pub fn flush(&mut self) -> Option<ContentChunk> {
        if self.buffer.is_empty() {
            return None;
        }
        let content = std::mem::take(&mut self.buffer);
        let ct = if self.current_close_tag.is_some() {
            ContentType::Thinking
        } else {
            ContentType::Text
        };
        Some(ContentChunk {
            content_type: ct,
            content,
        })
    }
}
