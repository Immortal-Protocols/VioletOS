#!/bin/sh
# Called by the one-way USB diagnostic stream on a known-working Linux boot.
bb=/bin/busybox
dt=/sys/firmware/devicetree/base
echo ELLIS-BOARD-BEGIN
for file in "$dt"/memory*/reg "$dt"/reserved-memory/*/reg "$dt"/chosen/atag,videolfb \
            "$dt"/chosen/linux,initrd-start "$dt"/chosen/linux,initrd-end; do
    [ -r "$file" ] || continue
    echo "DT ${file#"$dt"}"
    $bb od -An -v -tx1 "$file"
done
echo 'LINUX PHYSICAL RESOURCES'
$bb cat /proc/iomem
if [ ! -d /tmp/board-debug/memblock ]; then
    $bb mkdir -p /tmp/board-debug
    $bb mount -t debugfs -o ro debugfs /tmp/board-debug 2>/dev/null || :
fi
for file in /tmp/board-debug/memblock/memory /tmp/board-debug/memblock/reserved; do
    if [ -r "$file" ]; then echo "$file"; $bb cat "$file"; fi
done
for file in name stride bits_per_pixel virtual_size modes; do
    f=/sys/class/graphics/fb0/$file
    if [ -r "$f" ]; then echo "FB $file"; $bb cat "$f"; fi
done
/bin/fb-info
echo 'BOOT CPU MODE AND DISPLAY HANDOFF MESSAGES'
$bb dmesg -s 2097152 | $bb grep -Ei 'started in (SVC|HYP)|All CPU.s. started|framebuffer.*(base|reserved)|atag.videolfb|fb_base|vram.*(size|base)' | $bb tail -40
echo ELLIS-BOARD-END
