use mini_http_server::server::Server;
use mini_http_server::thread_pool::ThreadPool;

fn main() {
    let listener = std::net::TcpListener::bind("127.0.0.1:7878").expect("端口 7878 绑定失败");
    let pool = ThreadPool::new(4);
    println!("mini-http-server 已启动: http://127.0.0.1:7878 （4 个工作线程）");

    // incoming() 是迭代器：每 accept 到一条 TcpStream 就把连接处理任务丢进线程池
    // move 闭包拿走 stream 的所有权 → 编译器保证同一连接不会被两个线程同时持有
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("连接建立失败: {e}");
                continue;
            }
        };
        pool.execute(move || Server::handle_connection(stream));
    }
}
