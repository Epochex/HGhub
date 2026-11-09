use hghub_web::{initialize, make_app, Config};
use std::{net::SocketAddr, path::PathBuf};

/// @brief 启动独立网页服务，收到终止信号后优雅退出。
/// @return 配置、端口绑定或运行失败时返回错误。
#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binds = std::env::var("HGHUB_BIND").unwrap_or_else(|_| "127.0.0.1:8093".into());
    let hosts = std::env::var("HGHUB_ALLOWED_HOSTS").unwrap_or_else(|_| binds.clone());
    initialize(&PathBuf::from(
        std::env::var("HGHUB_DATA").unwrap_or_else(|_| ".web-data".into()),
    ))?;
    let app = make_app(Config {
        allowed_hosts: hosts.split(',').map(|s| s.trim().to_string()).collect(),
        dist: std::env::var("HGHUB_DIST")
            .unwrap_or_else(|_| "dist".into())
            .into(),
    });
    let mut tasks = tokio::task::JoinSet::new();
    for bind in binds.split(',') {
        let address: SocketAddr = bind.trim().parse()?;
        if address.ip().is_unspecified() {
            return Err("请明确指定私网监听地址".into());
        }
        let app = app.clone();
        tasks.spawn(async move {
            let listener = loop {
                match tokio::net::TcpListener::bind(address).await {
                    Ok(listener) => break listener,
                    Err(error) => {
                        eprintln!("Waiting for {address}: {error}");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            };
            println!("HGhub Web listening on http://{address}");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown())
                .await
        });
    }
    while let Some(result) = tasks.join_next().await {
        result??;
    }
    Ok(())
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("signal handler");
        tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
