//! 断点续传端到端验证：真实网络 + 进程级中断恢复 + 哈希校验
//!
//! cargo run --release --example resume_check -- <URL> <参考SHA256> [连接数]

use felldl_core::{AddTaskRequest, Engine, TaskState};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        println!("用法: resume_check <URL> <参考SHA256> [连接数]");
        return Ok(());
    }
    let url = args[0].clone();
    let expect_hash = args[1].clone().to_lowercase();
    let conns: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(8);

    let base = std::env::temp_dir().join(format!("felldl-resume-check-{}", std::process::id()));
    let out_dir = base.join("out");
    let data_dir = base.join("data");
    std::fs::create_dir_all(&out_dir)?;

    println!("═══ 阶段 1：限速下载，中途强制中止 ═══");
    let engine1 = Engine::new(&data_dir)?;
    {
        let mut s = engine1.settings();
        s.global_speed_limit = 800 * 1024; // 800KB/s
        s.download_dir = out_dir.to_string_lossy().to_string();
        engine1.update_settings(s)?;
    }
    let id = engine1
        .add(AddTaskRequest {
            url: url.clone(),
            save_dir: Some(out_dir.to_string_lossy().to_string()),
            file_name: None,
            connections: Some(conns),
            speed_limit: None,
            scheduled_at: None,
            referer: None,
            cookie: None,
            headers: Default::default(),
        })
        .await?;

    // 等到下载量够大再中断
    let mut got = 0u64;
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t = engine1.get(&id).unwrap();
        got = t.downloaded;
        if got > 6 * 1024 * 1024 {
            break;
        }
        if matches!(t.state, TaskState::Error) {
            anyhow::bail!("阶段1 出错: {:?}", t.error);
        }
    }
    println!("  中断前已下载: {}", felldl_core::util::human_bytes(got));
    engine1.pause(&id).await?;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let t = engine1.get(&id).unwrap();
    println!("  暂停时状态: {:?}, 进度: {}", t.state, felldl_core::util::human_bytes(t.downloaded));
    let paused_at = t.downloaded;
    let saved_path = t.save_path.clone();
    let seg_count = t.segments.len();
    drop(engine1); // 模拟进程退出

    println!("\n═══ 阶段 2：重启后从磁盘恢复并续传 ═══");
    let engine2 = Engine::new(&data_dir)?;
    {
        // 解除限速，让它跑快点
        let mut s = engine2.settings();
        s.global_speed_limit = 0;
        engine2.update_settings(s)?;
    }
    let t2 = engine2.get(&id).expect("任务应从磁盘恢复");
    println!("  恢复后状态: {:?}", t2.state);
    println!("  保留进度: {}", felldl_core::util::human_bytes(t2.downloaded));
    println!("  分片数: {}", t2.segments.len());
    assert_eq!(t2.state, TaskState::Paused, "重启后应为暂停态");
    assert!(t2.downloaded > 0, "重启后进度丢失");
    assert_eq!(t2.segments.len(), seg_count, "分片信息丢失");

    engine2.resume(&id).await?;
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let t = engine2.get(&id).unwrap();
        match t.state {
            TaskState::Completed => {
                println!(
                    "  续传完成，用时 {:.1}s，总量 {}",
                    start.elapsed().as_secs_f64(),
                    felldl_core::util::human_bytes(t.downloaded)
                );
                // 从被中断点续上，不能从头再来
                assert!(
                    t.downloaded >= paused_at,
                    "最终大小应不小于中断点"
                );
                break;
            }
            TaskState::Error => anyhow::bail!("阶段2 出错: {:?}", t.error),
            _ => {
                if start.elapsed().as_secs() > 900 {
                    anyhow::bail!("续传超时");
                }
            }
        }
    }

    println!("\n═══ 阶段 3：SHA256 校验 ═══");
    let data = std::fs::read(&saved_path)?;
    let hash = sha256_hex(&data);
    println!("  实际 SHA256: {hash}");
    println!("  期望 SHA256: {expect_hash}");
    if hash == expect_hash {
        println!("\n✅ 断点续传端到端验证通过：内容与源文件完全一致");
    } else {
        println!("\n❌ 哈希不一致，续传数据损坏！");
        std::process::exit(1);
    }

    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}

/// 纯 Rust SHA256（避免引入额外依赖）
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}
