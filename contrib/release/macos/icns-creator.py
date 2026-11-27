# To be run from the project root

import os
import shutil

from PIL import Image
from icnsutil import IcnsFile

# Input PNGs (should be at least 1024x1024)
# macOS: Big-Sur-style tile (squircle + margin + shadow), the line Cube on the dark tile.
input_file = "coincube-ui/static/logos/coincube-macos-1024.png"
# Pre-rendered sizes from design/icons/app/build_icons.py. The 16 and 32 px icons are drawn
# with a heavier line than a downscale of the 1024 tile would give, so use them when present.
iconset_dir = "coincube-ui/static/logos/macos-iconset"
# Windows: the multi-size .ico the exe already embeds, with the same small-size icons.
ico_source = "coincube-ui/static/logos/coincube-tenshu.ico"
output_file = "contrib/release/macos/Coincube.icns"
output_ico = "contrib/release/wix/Coincube.ico"

print(f"Processing {input_file}")

# Sizes macOS expects
sizes = [16, 32, 128, 256, 512]

img = Image.open(input_file).convert("RGBA")
icns = IcnsFile()

for size in sizes:
	# Standard size
	filename = f"contrib/icons/{size}x{size}.png"
	prerendered = f"{iconset_dir}/icon_{size}x{size}.png"
	if os.path.exists(prerendered):
		shutil.copyfile(prerendered, filename)
	else:
		img.resize((size, size), Image.LANCZOS).save(filename)

	print(f"Created {filename}")

	# Retina (@2x) size
	filename2x = f"contrib/icons/{size}x{size}@2x.png"
	prerendered2x = f"{iconset_dir}/icon_{size}x{size}@2x.png"
	if os.path.exists(prerendered2x):
		shutil.copyfile(prerendered2x, filename2x)
	else:
		img.resize((size*2, size*2), Image.LANCZOS).save(filename2x)

	print(f"Created {filename2x}")

	icns.add_media(file=filename)
	icns.add_media(file=filename2x)

# Write to filesystem
icns.write(output_file)

print(f"Created {output_file}")

# Windows ICO for the MSI: the same file the exe embeds
shutil.copyfile(ico_source, output_ico)
print(f"Created {output_ico}")
