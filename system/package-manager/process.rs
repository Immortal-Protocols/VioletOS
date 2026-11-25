// Recovery-plane process. Only Runtime can query its app catalog. The root
// supervisor alone can create processes or grant capabilities to their CSpaces.
use crate::protocol::*;
pub fn run() -> ! {
    serve(|m| match (m.op, m.key) {
        (GET, INVENTORY) => m.call(7),
        (GET, INBOX) | (EXECUTE, INSTALL | REMOVE) => m.call(7),
        (GET, CATALOG) => m.call(7),
        _ => Message::error(DENIED),
    })
}
