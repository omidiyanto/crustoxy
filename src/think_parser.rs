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
    in_think_tag: bool,
}

const OPEN_TAG: &str = "<think>";
const CLOSE_TAG: &str = "</think>";
const OPEN_TAG_LEN: usize = 7;
const CLOSE_TAG_LEN: usize = 8;

impl ThinkTagParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            in_think_tag: false,
        }
    }

    pub fn feed(&mut self, content: &str) -> Vec<ContentChunk> {
        self.buffer.push_str(content);
        let mut chunks = Vec::new();

        loop {
            let prev_len = self.buffer.len();
            let chunk = if !self.in_think_tag {
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
        let think_start = self.buffer.find(OPEN_TAG);
        let orphan_close = self.buffer.find(CLOSE_TAG);

        if let Some(oc) = orphan_close
            && (think_start.is_none() || oc < think_start.unwrap()) {
                let pre = self.buffer[..oc].to_string();
                self.buffer = self.buffer[oc + CLOSE_TAG_LEN..].to_string();
                if !pre.is_empty() {
                    return Some(ContentChunk {
                        content_type: ContentType::Text,
                        content: pre,
                    });
                }
                return None;
            }

        match think_start {
            None => {
                if let Some(last_bracket) = self.buffer.rfind('<') {
                    let potential = &self.buffer[last_bracket..];
                    let plen = potential.len();
                    if (plen < OPEN_TAG_LEN && OPEN_TAG.starts_with(potential))
                        || (plen < CLOSE_TAG_LEN && CLOSE_TAG.starts_with(potential))
                    {
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
            Some(pos) => {
                let pre = self.buffer[..pos].to_string();
                self.buffer = self.buffer[pos + OPEN_TAG_LEN..].to_string();
                self.in_think_tag = true;
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
        match self.buffer.find(CLOSE_TAG) {
            None => {
                if let Some(last_bracket) = self.buffer.rfind('<') {
                    let remaining = self.buffer.len() - last_bracket;
                    if remaining < CLOSE_TAG_LEN {
                        let potential = &self.buffer[last_bracket..];
                        if CLOSE_TAG.starts_with(potential) {
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
                self.buffer = self.buffer[pos + CLOSE_TAG_LEN..].to_string();
                self.in_think_tag = false;
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
        let ct = if self.in_think_tag {
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
