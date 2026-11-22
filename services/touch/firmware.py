"""Extract partition tables from the locally saved panel firmware (two DJN NT36525B revisions)."""
from pathlib import Path
import hashlib,struct,os
def partition(data):
    word=lambda pos: struct.unpack_from('<I',data,pos)[0]
    parts=[]
    for i in range(2):
        offset,addr,last=struct.unpack_from('<3I',data,i*12)
        parts.append((addr,offset,last+1,word(24+4*i)))
    assert data[32]&2==0 and data[40]&16==0
    for p in range(48,word(0),16):
        addr,last,offset,crc=struct.unpack_from('<4I',data,p)
        if last: parts.append((addr,offset,last+1,crc))
    for addr,offset,size,_ in parts:
        assert offset+size<=len(data) and addr+size<=0x30000
    return parts
def prepare(out, variant, stock_dir):
    alt=None
    if variant=="qemu":
        # Deterministic test bytes for the modeled controller. Keep several
        # transfer sizes and the two CRC banks used by the protocol tests.
        fw=out/"touch-fixture.bin"
        fw.write_bytes(bytes(range(256))*8)
        parts=[(0,0,1536,0x12345678),(0x27600,1536,512,0x87654321)]
    else:
        # Stock S3RHS32.20-42-13-2-3-2 firmware for the two DJN NT36525B panel revisions.
        # The first is the default; touch switches to the second when the loader reports
        # panel mipi_mot_vid_djn_nt36525b_hdp_652.
        touch=stock_dir/"touch/firmware"
        fw=Path(os.environ.get("VIOLET_TOUCH_FW",touch/"djn_nt36525b_a_novatek_ts_fw.bin")).resolve()
        alt=(touch/"djn_nt36525b_novatek_ts_fw.bin").resolve()
        for path,digest in ((fw,'7269992be538e72af63699947b545511cfe7cff482edabbd662c7e45cfe7e544'),
                            (alt,'5ee88aa50aa4d0262fb826f37dabef662233d943e9321c9595d0189de6ef07c3')):
            if not path.is_file():
                raise SystemExit(f"Missing {path.name}: copy the DJN NT36525B panel firmware (see README).")
            if hashlib.sha256(path.read_bytes()).hexdigest()!=digest:
                raise ValueError(f'unexpected panel firmware {path.name}')
        parts=partition(fw.read_bytes())
    rows=',\n'.join('{'+','.join(map(hex,p))+'}' for p in parts)
    (out/'firmware.h').write_text('static const struct { unsigned addr,offset,size,crc; } parts[]={\n'+rows+'\n};\n')
    import json
    table=lambda p: "&[" + ",".join(map(str,p)) + "]"
    alt_rs=(f"static TOUCH_FIRMWARE_ALT: [u8;{alt.stat().st_size}] = *include_bytes!({json.dumps(str(alt))});\n"
            + f"static ALT: Option<(&[u8], &[(usize,usize,usize,u32)])> = Some((&TOUCH_FIRMWARE_ALT, {table(partition(alt.read_bytes()))}));\n"
            if alt else "static ALT: Option<(&[u8], &[(usize,usize,usize,u32)])> = None;\n")
    (out/'firmware.rs').write_text(
        f"#[no_mangle] static touch_firmware: [u8;{fw.stat().st_size}] = *include_bytes!({json.dumps(str(fw))});\n"
        + f"const PARTS: &[(usize,usize,usize,u32)] = {table(parts)};\n" + alt_rs)
