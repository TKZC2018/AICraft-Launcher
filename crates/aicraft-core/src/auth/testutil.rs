//! 测试用的假 HTTP 层：按顺序吐出预设响应，并记录每次请求。
//!
//! 登录链路的每一跳都可以用它离线跑通——不需要网、不需要 client id、
//! 也不会真的弹浏览器（回环握手那一步在 `msa` 的测试里单独模拟）。

use std::cell::RefCell;
use std::collections::VecDeque;

use crate::transport::{FetchError, HttpResponse};

use super::Http;

/// 一次被记录的请求：`(方法, URL, 正文)`。
pub struct MockHttp {
    queue: RefCell<VecDeque<HttpResponse>>,
    pub log: RefCell<Vec<(String, String, String)>>,
}

impl MockHttp {
    pub fn new(responses: Vec<HttpResponse>) -> Self {
        MockHttp {
            queue: RefCell::new(responses.into()),
            log: RefCell::new(Vec::new()),
        }
    }

    fn next(&self, method: &str, url: &str, body: &str) -> Result<HttpResponse, FetchError> {
        self.log
            .borrow_mut()
            .push((method.to_string(), url.to_string(), body.to_string()));
        self.queue
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| FetchError {
                message: format!("mock 没有更多响应：{} {}", method, url),
                exit_code: None,
            })
    }
}

impl Http for MockHttp {
    fn post_form(&self, url: &str, pairs: &[(&str, &str)]) -> Result<HttpResponse, FetchError> {
        self.next("POST", url, &crate::codec::urlencode_pairs(pairs))
    }

    fn post_json(&self, url: &str, body: &str) -> Result<HttpResponse, FetchError> {
        self.next("POST", url, body)
    }

    fn get(&self, url: &str, _headers: &[(&str, &str)]) -> Result<HttpResponse, FetchError> {
        self.next("GET", url, "")
    }
}
