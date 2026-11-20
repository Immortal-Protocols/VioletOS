// Check the initial AArch64 stage-2 mappings used by this two-domain fixture.
use sel4_capdl_initializer_types::{
    ArchivedCap as Cap, ArchivedObject as Object, SpecForInitializer,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs,
};

fn main() -> Result<(), Box<dyn Error>> {
    let file = env::args().nth(1).ok_or("supply extracted CapDL payload")?;
    let bytes = fs::read(file)?;
    let spec = SpecForInitializer::access(&bytes)?;
    let mappings = |domain: &str| -> Result<BTreeMap<u64, (usize, bool, bool)>, Box<dyn Error>> {
        let name = format!("tcb_{domain}");
        let mut tcbs = spec
            .objects
            .iter()
            .filter(|o| o.name.as_ref().is_some_and(|n| n.as_str() == name));
        let Object::Tcb(tcb) = &tcbs.next().ok_or("TCB missing")?.object else {
            return Err("invalid TCB".into());
        };
        if tcbs.next().is_some() {
            return Err("duplicate TCB name".into());
        }
        let root = tcb
            .slots
            .iter()
            .find(|s| usize::from(s.slot) == 1)
            .ok_or("no VSpace")?;
        let Cap::PageTable(root) = &root.cap else {
            return Err("invalid VSpace".into());
        };
        // Audit reachable capability slots too. In this fixture neither service
        // should hold a frame cap, untyped allocator, endpoint to a peer,
        // thread-control cap, or authority over another address space.
        let croot = tcb
            .slots
            .iter()
            .find(|s| usize::from(s.slot) == 0)
            .ok_or("no CSpace")?;
        let Cap::CNode(croot) = &croot.cap else {
            return Err("invalid CSpace".into());
        };
        let mut cnodes = vec![usize::from(croot.object)];
        let mut seen = BTreeSet::new();
        while let Some(id) = cnodes.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Object::CNode(node) = &spec.objects.get(id).ok_or("invalid CNode ID")?.object
            else {
                return Err("invalid CNode".into());
            };
            for entry in node.slots.iter() {
                match &entry.cap {
                    Cap::CNode(c) => cnodes.push(usize::from(c.object)),
                    Cap::PageTable(c) if c.object == root.object => {}
                    Cap::Notification(c)
                        if !c.rights.write && !c.rights.grant && !c.rights.grant_reply => {}
                    Cap::Reply(c) if !c.rights.grant && !c.rights.grant_reply => {}
                    _ => return Err("capability outside the two-service fixture policy".into()),
                }
            }
        }
        let mut todo = vec![(usize::from(root.object), 1u8, 0u64)];
        let mut result = BTreeMap::new();
        while let Some((id, level, prefix)) = todo.pop() {
            let Object::PageTable(pt) = &spec.objects.get(id).ok_or("invalid table ID")?.object
            else {
                return Err("invalid table".into());
            };
            if pt.level.as_ref() != Some(&level) {
                return Err("unexpected page-table level".into());
            }
            for s in pt.slots.iter() {
                let index = usize::from(s.slot);
                let entries = if level == 1 { 1024 } else { 512 };
                if index >= entries {
                    return Err("invalid page-table slot".into());
                }
                let va = prefix | ((index as u64) << (12 + 9 * (3 - level)));
                match &s.cap {
                    Cap::PageTable(c) if level < 3 => {
                        todo.push((usize::from(c.object), level + 1, va))
                    }
                    Cap::Frame(c) if level == 3 => {
                        if result
                            .insert(va, (usize::from(c.object), c.rights.read, c.rights.write))
                            .is_some()
                        {
                            return Err("duplicate mapping".into());
                        }
                    }
                    _ => return Err("unsupported mapping in this fixture".into()),
                }
            }
        }
        Ok(result)
    };
    let notes = mappings("Notes")?;
    let net = mappings("Network")?;
    let &(note, true, true) = notes.get(&0x40000000).ok_or("owner mapping missing")? else {
        return Err("invalid owner rights".into());
    };
    let &(packet, true, true) = net.get(&0x41000000).ok_or("packet mapping missing")? else {
        return Err("invalid packet rights".into());
    };
    if packet == note {
        return Err("packet aliases the note".into());
    }
    for (va, &(id, read, write)) in &net {
        if id == note && (read || write) {
            eprintln!(
                "REJECT: Network maps Notes' private frame at {va:#x} (read={read}, write={write})"
            );
            std::process::exit(2)
        }
    }
    println!("PASS: Notes owns its writable page; Network has a separate packet page and no mapping to the note frame");
    Ok(())
}
