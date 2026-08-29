use std::io::{BufReader, Write};
use std::net::TcpStream;

use crate::http::{build_response, parse_request, Request, Status};

pub struct Server;

impl Server {
    pub fn handle_connection(stream: TcpStream) {
        let peer = stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "未知对端".to_string());

        // 同一条连接一读一写：try_clone 复制一个底层句柄给 BufReader，
        // 原句柄留给写响应——两个所有权互不干扰，离开作用域各自释放
        let mut reader = BufReader::new(stream.try_clone().expect("克隆 TcpStream 失败"));

        let response = match parse_request(&mut reader) {
            Ok(req) => Self::route(&req),
            Err(e) => {
                eprintln!("[{peer}] 请求解析失败: {e}");
                build_response(
                    Status::BadRequest,
                    "text/plain; charset=utf-8",
                    "400 Bad Request\n",
                )
            }
        };

        let mut stream = stream;
        let status_line = response.lines().next().unwrap_or_default().to_string();
        match stream.write_all(response.as_bytes()).and_then(|_| stream.flush()) {
            Ok(_) => println!("[{peer}] {status_line}"),
            Err(e) => eprintln!("[{peer}] 响应写入失败: {e}"),
        }
    }

    /// 路由：元组 + match 的穷尽匹配，漏写分支编译不过
    fn route(req: &Request) -> String {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") => build_response(
                Status::Ok,
                "text/html; charset=utf-8",
                Self::index_page().as_str(),
            ),
            ("GET", "/sleep") => {
                // 慢请求模拟：占用一个工作线程 5 秒。
                // 同时再开浏览器访问 / 不会被阻塞——这正是线程池的意义
                std::thread::sleep(std::time::Duration::from_secs(5));
                build_response(
                    Status::Ok,
                    "text/plain; charset=utf-8",
                    "睡醒了（这个请求耗时 5 秒）\n",
                )
            }
            (_, path) => build_response(
                Status::NotFound,
                "text/html; charset=utf-8",
                format!("<h1>404</h1><p>没有 {path} 这个路由</p>").as_str(),
            ),
        }
    }

    fn index_page() -> String {
        "<html><head><title>mini-http-server</title></head><body>\
         <h1>Hello, Rust!</h1>\
         <p>纯标准库手写的多线程 HTTP 服务器。</p>\
         <ul>\
         <li><a href=\"/\">GET /</a>：本页</li>\
         <li><a href=\"/sleep\">GET /sleep</a>：耗时 5 秒的慢请求（试试慢请求期间刷新首页）</li>\
         </ul></body></html>"
            .to_string()
    }
}
