//! Desktop Prism Studio entry point.

fn main() {
    let command = match prism_studio::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("Prism Studio: {error}");
            std::process::exit(2);
        }
    };
    match prism_studio::run(command) {
        Ok(Some(summary)) => println!("{summary}"),
        Ok(None) => {
            println!("Prism Studio shell ready; graphical workspace integration is pending.");
        }
        Err(error) => {
            eprintln!("Prism Studio: {error}");
            std::process::exit(1);
        }
    }
}
