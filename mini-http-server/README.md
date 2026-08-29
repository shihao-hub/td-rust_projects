# mini-http-server

纯 Rust 标准库实现的多线程 HTTP 服务器，零第三方依赖。用来练手所有权、`Result` 错误建模、`Arc<Mutex<T>>` 共享状态和 RAII 式资源回收。

## 快速开始

```bash
cd mini-http-server
cargo run
```

启动后监听 `http://127.0.0.1:7878`，默认 4 个工作线程。

```bash
cargo test    # 6 个单元测试
```

## 路由

| 方法 | 路径 | 行为 |
| --- | --- | --- |
| GET | `/` | 返回首页 HTML |
| GET | `/sleep` | 线程内 `sleep(5s)` 后返回，模拟慢请求 |
| 其他 | 任意 | 404 HTML |

请求行解析失败返回 400。

验证线程池效果：先打开 `/sleep`，在它返回之前再开一个标签访问 `/`，首页会立刻响应。把线程数调成 1 就能看到第二个请求被阻塞。

## 结构

```
src/
├── main.rs         # 绑定端口，accept 循环，把连接投进线程池
├── lib.rs          # 模块声明
├── http.rs         # Request / Status / ParseError，请求解析与响应拼装
├── server.rs       # 单连接处理与路由
└── thread_pool.rs  # 固定大小线程池
```

数据流：`TcpListener::incoming()` → `pool.execute(move || ...)` → `Server::handle_connection` → `parse_request` → `route` → `build_response` → 写回 socket。

## 实现要点

**连接读写分离**（`server.rs:17`）。`BufReader` 需要拥有一个 `TcpStream`，写响应也需要一个，所以用 `try_clone()` 复制底层句柄：两份所有权互不干扰，各自离开作用域时释放。

**错误用枚举表达**（`http.rs:42`）。`ParseError` 覆盖空请求、请求行非法、IO 失败三种情况。实现 `From<io::Error>` 后，解析函数里可以直接用 `?` 让 `io::Error` 自动转换。

**线程池的共享接收端**（`thread_pool.rs:24`）。mpsc 的 `Receiver` 只允许单个消费者，要让 N 个 Worker 抢同一个队列就得套 `Arc<Mutex<Receiver>>`：`Arc` 提供共享所有权，`Mutex` 串行化取任务。少任何一层都编译不过。`lock()` 返回的 `MutexGuard` 在作用域结束自动解锁，`recv()` 在空队列时阻塞休眠而非忙等。

**关闭顺序**（`thread_pool.rs:50`）。`Drop` 里先 `self.sender.take()` 丢掉发送端，Worker 的 `recv()` 收到 `Err` 后跳出循环；然后再逐个 `join`。顺序反了会死锁。因此 `drop(pool)` 返回时，所有已投递任务保证执行完毕——`thread_pool.rs:107` 的测试就是在断言这一点。

**路由的穷尽匹配**（`server.rs:41`）。`match (method, path)` 元组匹配，`(_, path)` 兜底分支必须存在，否则编译器报错。

## 已知限制

这是教学项目，不适合生产：

- 只解析请求行和头部，忽略 body，不支持 POST 语义
- `Connection: close`，每个请求一条连接，无 keep-alive
- 没有读超时，恶意慢连接可以长期占住一个工作线程
- 无静态文件服务、无 TLS、无请求体大小限制
- 线程数硬编码为 4，端口硬编码为 7878

监听地址是 `127.0.0.1`，仅本机可访问。改成 `0.0.0.0` 会暴露到局域网，而当前实现没有任何认证、限流或超时保护，别这么做。
