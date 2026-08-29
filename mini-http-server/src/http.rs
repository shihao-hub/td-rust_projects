use std::collections::HashMap;
use std::fmt;
use std::io::BufRead;

/// 一次 HTTP 请求的结构化表示
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub version: String,
    pub headers: HashMap<String, String>,
}

/// 响应状态码：枚举 + match 穷尽匹配，写错分支编译器直接报错
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    NotFound,
    BadRequest,
}

impl Status {
    pub fn code(self) -> u16 {
        match self {
            Status::Ok => 200,
            Status::NotFound => 404,
            Status::BadRequest => 400,
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Status::Ok => "OK",
            Status::NotFound => "Not Found",
            Status::BadRequest => "Bad Request",
        }
    }
}

/// 解析失败只用枚举表达，没有异常：调用方被编译器强制处理每一种错误
#[derive(Debug)]
pub enum ParseError {
    EmptyRequest,
    MalformedRequestLine(String),
    Io(std::io::Error),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::EmptyRequest => write!(f, "空请求"),
            ParseError::MalformedRequestLine(line) => write!(f, "请求行格式非法: {line}"),
            ParseError::Io(e) => write!(f, "读取请求失败: {e}"),
        }
    }
}

/// ? 运算符自动转换：io::Error 通过这个 From 实现变成 ParseError::Io
impl From<std::io::Error> for ParseError {
    fn from(e: std::io::Error) -> Self {
        ParseError::Io(e)
    }
}

/// 从字节流解析 HTTP 请求（请求行 + 头部，到空行为止）
pub fn parse_request(reader: &mut dyn BufRead) -> Result<Request, ParseError> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 || request_line.trim().is_empty() {
        return Err(ParseError::EmptyRequest);
    }

    let mut parts = request_line.trim_end().splitn(3, ' ');
    let (method, path, version) = match (parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(p), Some(v)) => (m.to_string(), p.to_string(), v.to_string()),
        _ => {
            return Err(ParseError::MalformedRequestLine(
                request_line.trim().to_string(),
            ))
        }
    };

    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    Ok(Request {
        method,
        path,
        version,
        headers,
    })
}

/// 拼一个最简合法的 HTTP/1.1 响应
pub fn build_response(status: Status, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status.code(),
        status.reason(),
        content_type,
        body.len(),
        body
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn parse(raw: &str) -> Result<Request, ParseError> {
        parse_request(&mut BufReader::new(raw.as_bytes()))
    }

    #[test]
    fn parses_get_request_with_headers() {
        let req =
            parse("GET /hello HTTP/1.1\r\nHost: example.com\r\nUser-Agent: test\r\n\r\n").unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/hello");
        assert_eq!(req.version, "HTTP/1.1");
        assert_eq!(
            req.headers.get("host").map(String::as_str),
            Some("example.com")
        );
    }

    #[test]
    fn rejects_empty_request() {
        assert!(matches!(parse(""), Err(ParseError::EmptyRequest)));
        assert!(matches!(parse("\r\n"), Err(ParseError::EmptyRequest)));
    }

    #[test]
    fn rejects_malformed_request_line() {
        assert!(matches!(
            parse("GETONLY\r\n\r\n"),
            Err(ParseError::MalformedRequestLine(_))
        ));
    }

    #[test]
    fn builds_response_with_correct_headers() {
        let resp = build_response(Status::NotFound, "text/html", "nope");
        assert!(resp.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(resp.contains("Content-Length: 4"));
        assert!(resp.ends_with("nope"));
    }
}
