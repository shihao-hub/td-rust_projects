use std::sync::{mpsc, Arc, Mutex};
use std::thread;

/// 任务 = 堆上的闭包。三个 trait 约束就是 Rust 并发安全的核心：
/// - FnOnce：任务只被执行一次，用掉闭包捕获的所有权
/// - Send：闭包及其捕获的变量可安全跨线程转移所有权
/// - 'static：闭包不借用栈上短命数据，线程活得比投递者久也不会悬垂
type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct ThreadPool {
    workers: Vec<Worker>,
    sender: Option<mpsc::Sender<Job>>,
}

impl ThreadPool {
    /// 创建固定大小线程池；size 为 0 直接 panic（契约前置检查）
    pub fn new(size: usize) -> ThreadPool {
        assert!(size > 0, "线程池大小必须大于 0");

        let (sender, receiver) = mpsc::channel();
        // mpsc = multi-producer single-consumer：接收端天生只允许一个消费者。
        // 要让 N 个 Worker 共享，必须 Arc（共享所有权）+ Mutex（串行化访问），
        // 少包任何一层编译器都直接拒绝——这就是" fearless concurrency "
        let receiver = Arc::new(Mutex::new(receiver));

        let workers = (0..size)
            .map(|id| Worker::new(id, Arc::clone(&receiver)))
            .collect();

        ThreadPool {
            workers,
            sender: Some(sender),
        }
    }

    pub fn execute<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let job = Box::new(f);
        if let Some(sender) = &self.sender
            && sender.send(job).is_err()
        {
            eprintln!("任务投递失败：线程池已关闭");
        }
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        // 关闭顺序是关键：先 drop 发送端 → 所有 Worker 的 recv() 收到 Err 自然退出，
        // 然后才 join 等它们跑完手头任务。Option::take 把 sender 从 &mut self 里"借"出来 drop
        drop(self.sender.take());
        for worker in &mut self.workers {
            if let Some(handle) = worker.handle.take()
                && let Err(e) = handle.join()
            {
                eprintln!("Worker {} join 失败: {:?}", worker.id, e);
            }
        }
    }
}

struct Worker {
    id: usize,
    handle: Option<thread::JoinHandle<()>>,
}

impl Worker {
    fn new(id: usize, receiver: Arc<Mutex<mpsc::Receiver<Job>>>) -> Worker {
        // move 把 receiver 的所有权移进线程闭包；Arc::clone 只加引用计数不复制数据
        let handle = thread::spawn(move || loop {
            // lock() 返回 MutexGuard（RAII 守卫）：作用域结束自动解锁，忘记解锁不可能发生。
            // recv() 拿不到锁就睡眠等待，不忙转
            let job = receiver.lock().unwrap().recv();
            match job {
                Ok(job) => {
                    println!("Worker {id} 接到任务");
                    job();
                    println!("Worker {id} 完成任务");
                }
                Err(_) => {
                    println!("Worker {id} 收到关闭信号，退出");
                    break;
                }
            }
        });
        Worker {
            id,
            handle: Some(handle),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    #[should_panic(expected = "线程池大小必须大于 0")]
    fn new_panics_on_zero_size() {
        ThreadPool::new(0);
    }

    #[test]
    fn executes_all_jobs_and_shuts_down_cleanly() {
        let pool = ThreadPool::new(4);
        let done = Arc::new(AtomicUsize::new(0));
        for _ in 0..100 {
            let done = Arc::clone(&done);
            pool.execute(move || {
                done.fetch_add(1, Ordering::SeqCst);
            });
        }
        // Drop 里会先关闭 channel 再 join 全部 Worker：走完这一行，100 个任务保证执行完毕
        drop(pool);
        assert_eq!(done.load(Ordering::SeqCst), 100);
    }
}
