# KON

KON is a Windows desktop application for lossless compression of images and RAW photographs.

It uses two specialized compression algorithms:

- **ELLIE** (codec 6) for PNG and JPEG input.
- **ULISA** (codec 7) for supported Bayer RAW DNG input.

Compressed files use the `.pkon` format. You can open them in KON's built-in **PKON Viewer**.

## Features

- Compress individual files or whole folders (subfolders are included).
- Lossless: after compressing, KON decodes the result and checks that it matches the input before reporting success.
- **PNG and JPEG** input, compressed with ELLIE.
- **Bayer RAW DNG** input, compressed with ULISA. Supported DNGs are single-plane, 1–16-bit, and either uncompressed or lossless-JPEG compressed. Other DNG variants and other RAW formats (CR2, NEF, ARW, and so on) are rejected with an error message.
- **PKON Viewer** to open and decode `.pkon` files, see their details, preview the image, and export it as PNG.
- **Compare** a decoded `.pkon` against its original, in slider or side-by-side mode.
- **Camera-aware RAW rendering** when the original DNG is available (see below).

### What "lossless" means here

- **PNG:** the decoded pixels match the original pixels exactly.
- **JPEG:** the decoded pixels match the JPEG's *decoded* pixels exactly. The original JPEG file itself, including EXIF and ICC data, cannot be recovered from the `.pkon`.
- **RAW DNG:** KON stores the DNG's raw sensor samples (plus the color-filter layout needed to decode them). The `.pkon` is **not** a DNG and does not preserve the DNG's tags, EXIF, embedded previews, or maker notes. Keep your original DNGs if you need that information.

### RAW previews are renderings

RAW sensor data is not a viewable picture. To show or export a RAW `.pkon`, the Viewer has to "develop" it (demosaic, white balance, color conversion). That image is a rendering, not the stored data.

- If the original DNG is available (for example, you just compressed it in this session, or you choose it in Compare), KON uses the camera's color information from that DNG for a **camera-aware** rendering.
- If not, the `.pkon` has no camera color data, so KON shows a clearly labeled **generic preview** that will not match the camera's colors.

## How to use KON

### Compress

1. Open the **Compress** tab.
2. Add files with **Add Files** or **Add Folder**, or drag and drop them into the window. **Try sample** loads a bundled sample DNG.
3. Select a file in the list to see its details.
4. Under **Output folder**, click **Choose…** and pick where the `.pkon` files should go.
5. Click **Start compression**. Use **Cancel remaining** to stop queued files (a file already in progress finishes).

Each file shows its result. Files KON cannot handle are marked as failed with the reason.

### View

1. Open the **PKON Viewer** tab.
2. Use **Open Files** or **Open Folder**, or drop `.pkon` files in.
3. Select a file to decode and preview it, and to see its container details.
4. Click **Export as PNG…** to save the previewed image.

### Compare

1. In the Viewer, select a `.pkon` and click **Compare…**.
2. KON uses the original it remembers from this session. Otherwise it asks you to select the original image. The `.pkon` format does not store the original's filename.
3. Switch between **Slider** and **Side by side**, and use Fit / 100% / 200% to zoom. RAW comparisons can take several seconds to load.

## Windows security warning

The current KON executable is **unsigned**: it has no code-signing certificate. When you launch it, Windows SmartScreen may show "Windows protected your PC" or an unfamiliar-app warning. This happens because Windows cannot verify a publisher for unsigned programs; it does not by itself mean the file is harmful.

Before choosing to proceed, make sure you got the executable from the official KON project source, the GitHub repository: <https://github.com/Kernel-of-Optimized-Networks-KON/kon-app>. If you are not sure where your copy came from, don't run it.

Please don't turn off Windows security features globally to run KON. If you have verified the source, you can choose to run this one program from the SmartScreen prompt (**More info**, then **Run anyway**).

## Platform support

- **Windows** is the current supported platform.
- **macOS** has not been tested.
- **Linux** has not been tested.

Please don't assume KON works on anything other than Windows.

## Reporting bugs

Open an issue on the repository's GitHub Issues page: <https://github.com/Kernel-of-Optimized-Networks-KON/kon-app/issues>

Please include:

- KON version
- Windows version
- Input file format and approximate size
- Steps to reproduce the problem
- What you expected to happen and what actually happened
- Any error messages or screenshots, if you can

**Privacy:** do not upload private photographs or sensitive files to a public issue. Describe the problem instead, or attach a small non-sensitive sample that shows it.

## Roadmap

- **Audio compression:** planned future support for lossless audio compression. This is **not currently available**; KON today handles images and RAW photographs only.

Future directions will be announced as they become concrete.

## License

KON is licensed under the [PolyForm Noncommercial License 1.0.0](LICENSE). It is a noncommercial license and is not an open-source license. See [LICENSE](LICENSE) for the full terms.
