# Join several screenshots side by side (each cropped to max height) so one Read shows several.
import sys
from PIL import Image
out=sys.argv[1]; maxh=int(sys.argv[2]); files=sys.argv[3:]
ims=[]
for f in files:
    off=0
    if ':' in f: f,off=f.split(':'); off=int(off)
    im=Image.open(f).convert('RGB'); im=im.crop((0,off,im.width,min(im.height,off+maxh))); ims.append(im)
W=sum(i.width for i in ims)+12*(len(ims)-1); H=max(i.height for i in ims)
c=Image.new('RGB',(W,H),(255,0,255)); x=0
for i in ims: c.paste(i,(x,0)); x+=i.width+12
c.save(out); print(out,c.size)
