// Recovery-plane process. Only Runtime can query its app catalog. The root
// supervisor alone can create processes or grant capabilities to their CSpaces.
use crate::protocol::*;
pub fn run() -> ! {
    let available = Message::new(GET, INVENTORY, 0).call(7).value;
    serve(|m| match (m.op, m.key) {
        (GET, INVENTORY) => Message::result(available),
        (GET, CATALOG) => {
            let app = if m.value == WIFI { WIFI } else { WATCH };
            let mut r = Message::result(if available & (1 << app) != 0 { app } else { 0 });
            if r.value != 0 {
                r.put_text(if app == WIFI {
                    env!("VIOLET_WIFI_TITLE").as_bytes()
                } else {
                    env!("VIOLET_WATCH_TITLE").as_bytes()
                });
            }
            r
        }
        _ => Message::error(DENIED),
    })
}
