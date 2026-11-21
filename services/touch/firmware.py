"""Extract the partition table from the single supported, locally saved firmware."""
from pathlib import Path
import hashlib,struct,os
def prepare(out, variant, stock_dir):
    if variant=="qemu":
        # Deterministic test bytes for the modeled controller. Keep several
        # transfer sizes and the two CRC banks used by the protocol tests.
        fw=out/"touch-fixture.bin"
        fw.write_bytes(bytes(range(256))*8)
        parts=[(0,0,1536,0x12345678),(0x27600,1536,512,0x87654321)]
    else:
        fw=Path(os.environ.get("VIOLET_TOUCH_FW",stock_dir/"touch/firmware/djn_nt36525b_a_novatek_ts_fw.bin")).resolve()
        if not fw.is_file():
            raise SystemExit("Set VIOLET_TOUCH_FW to the DJN NT36525B-A panel firmware (see README).")
        data=fw.read_bytes()
        if hashlib.sha256(data).hexdigest()!='7269992be538e72af63699947b545511cfe7cff482edabbd662c7e45cfe7e544':
            raise ValueError('unexpected panel firmware')
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
    rows=',\n'.join('{'+','.join(map(hex,p))+'}' for p in parts)
    (out/'firmware.h').write_text('static const struct { unsigned addr,offset,size,crc; } parts[]={\n'+rows+'\n};\n')
    import json
    (out/'firmware.rs').write_text(
        f"#[no_mangle] static touch_firmware: [u8;{fw.stat().st_size}] = *include_bytes!({json.dumps(str(fw))});\n"
        + "const PARTS: &[(usize,usize,usize,u32)] = &[" + ",".join(map(str,parts)) + "];\n")
