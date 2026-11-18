use miharashi_daidai_inbox::config::{USAGE, parse_args};
use miharashi_daidai_inbox::server::Server;

fn main() {
    let cfg = match parse_args(std::env::args().skip(1)) {
        Ok(Some(c)) => c,
        Ok(None) => {
            println!("{USAGE}");
            return;
        }
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let allow = cfg.allow.len();
    let server = match Server::bind(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("起動できません: {e}");
            std::process::exit(2);
        }
    };
    match server.local_addr() {
        Ok(a) => eprintln!("待ち受け開始: {a}(許可する送信元 {allow} 件)"),
        Err(_) => eprintln!("待ち受け開始"),
    }
    server.run();
}
