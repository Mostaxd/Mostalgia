"""Packs rasterized frames (png/<character>/<pose>-NN.png) into animated WebP sprites."""
import collections, glob, os, sys
from PIL import Image

src, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
for ch in sorted(os.listdir(src)):
    groups = collections.defaultdict(list)
    for f in sorted(glob.glob(f'{src}/{ch}/*.png')):
        groups[os.path.basename(f).rsplit('-', 1)[0]].append(f)
    for pose, files in groups.items():
        frames = [Image.open(f).convert('RGBA') for f in files]
        dest = f'{out}/{ch}-{pose}.webp'
        # the app draws every pose at 12 fps
        frames[0].save(dest, save_all=True, append_images=frames[1:], duration=83, loop=0, quality=82, method=6)
        print(dest, len(frames), 'frames')
