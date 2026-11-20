"""Extract the partition table from the single supported, locally saved firmware."""
from pathlib import Path
import hashlib,struct
def prepare(repo,out):
    fw=next((repo/'build/xt2163dl').glob('*/stock/touch/firmware/djn_nt36525b_a_novatek_ts_fw.bin'))
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
    (out/'firmware.S').write_text('.section .rodata.firmware,"a"\n.global touch_firmware\ntouch_firmware: .incbin "'+str(fw)+'"\n')
