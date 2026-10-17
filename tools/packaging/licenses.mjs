import { readFileSync, readdirSync, existsSync, mkdirSync, copyFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { root, capture, readJson, sha256, writeJson, target } from './common.mjs';

const out = path.join(root, 'package-resources');
mkdirSync(out, { recursive: true });
const components = [];
const missing = [];
const overrides = readJson(path.join(root, 'tools/packaging/license-overrides.json'));
function licenseFiles(directory, depth = 0) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isFile() && /^(licen[sc]e|copying|notice|unlicense|copyright|ofl)([._-]|$)/i.test(entry.name)) {
      files.push(path.join(directory, entry.name));
    } else if (entry.isDirectory() && depth < 2 && /^(licen[sc]es?|legal|fonts?|vendor)$/i.test(entry.name)) {
      files.push(...licenseFiles(path.join(directory, entry.name), depth + 1));
    }
  }
  return files;
}
function collect(component, directory, extraFiles = []) {
  const id = `${component.ecosystem}-${component.name}-${component.version}`.replace(/[^a-zA-Z0-9._-]/g, '_');
  const files = [...new Set([...(directory ? licenseFiles(directory) : []), ...extraFiles])];
  const override = overrides[`${component.ecosystem}:${component.name}@${component.version}`];
  if (override) {
    for (const file of override.files) {
      const source = path.join(root, 'docs/licenses/package-overrides', file.name);
      if (sha256(source) !== file.sha256) throw new Error(`License override changed: ${source}`);
      files.push(source);
    }
    component.licenseSources = override.files;
    if (override.note) component.licenseNote = override.note;
  }
  if (!component.license || !files.length) missing.push(`${component.name}@${component.version}: license=${component.license}, texts=${files.length}`);
  const texts = [];
  for (const [index, source] of files.entries()) {
    const destination = path.join('texts', id, `${index}-${path.basename(source)}`);
    mkdirSync(path.join(out, path.dirname(destination)), { recursive: true });
    copyFileSync(source, path.join(out, destination));
    texts.push({ file: destination.replaceAll('\\', '/'), sha256: sha256(source) });
  }
  components.push({ ...component, texts });
}

const npmLock = readJson(path.join(root, 'package-lock.json'));
for (const [relative, locked] of Object.entries(npmLock.packages)) {
  if (!relative || !existsSync(path.join(root, relative, 'package.json'))) continue;
  const directory = path.join(root, relative);
  const pkg = readJson(path.join(directory, 'package.json'));
  // Platform binary packages omit duplicate notices; use the same-version parent
  // package's original license texts. They are build tools, not app resources.
  const parentName = pkg.name.startsWith('@esbuild/') ? 'esbuild'
    : pkg.name.startsWith('@rollup/rollup-') ? 'rollup'
    : pkg.name.startsWith('@tauri-apps/cli-') ? '@tauri-apps/cli' : undefined;
  let parentTexts = [];
  if (parentName) {
    const parentDir = path.join(root, 'node_modules', parentName);
    if (readJson(path.join(parentDir, 'package.json')).version !== pkg.version) throw new Error(`License parent version differs: ${pkg.name}`);
    parentTexts = licenseFiles(parentDir);
  }
  collect({ ecosystem: 'npm', name: pkg.name, version: pkg.version,
    license: pkg.license ?? locked.license, source: locked.resolved, integrity: locked.integrity,
    scope: locked.dev ? 'build-tool' : 'application dependency', repository: pkg.repository }, directory, parentTexts);
}
const metadata = JSON.parse(capture('cargo', ['metadata', '--locked', '--format-version', '1',
  '--manifest-path', 'src-tauri/Cargo.toml', '--filter-platform', target()], { maxBuffer: 20 * 1024 * 1024 }));
for (const pkg of metadata.packages) {
  if (pkg.name === 'cuenote') continue;
  const directory = path.dirname(pkg.manifest_path);
  collect({ ecosystem: 'cargo', name: pkg.name, version: pkg.version,
    license: pkg.license, authors: pkg.authors, source: `https://crates.io/api/v1/crates/${pkg.name}/${pkg.version}/download`,
    repository: pkg.repository, scope: 'resolved target graph (includes build/test tools)' }, directory,
  pkg.license_file ? [path.resolve(directory, pkg.license_file)] : []);
}
// KaTeX's JavaScript is MIT, but the actual bundled fonts declare OFL-1.1.
// Read their embedded copyright / reserved-name notices instead of guessing
// font licensing from the npm package-level license field.
const fontDir = path.join(root, 'node_modules/katex/dist/fonts');
const fontNotices = [];
for (const file of readdirSync(fontDir).filter(f => f.endsWith('.ttf'))) {
  const bytes = readFileSync(path.join(fontDir, file));
  let nameOffset;
  for (let i = 0; i < bytes.readUInt16BE(4); i++) {
    const record = 12 + i * 16;
    if (bytes.toString('ascii', record, record + 4) === 'name') nameOffset = bytes.readUInt32BE(record + 8);
  }
  if (!nameOffset) throw new Error(`Font has no name table: ${file}`);
  const notices = new Set();
  for (let i = 0; i < bytes.readUInt16BE(nameOffset + 2); i++) {
    const record = nameOffset + 6 + i * 12;
    if (![0, 13, 14].includes(bytes.readUInt16BE(record + 6))) continue;
    const start = nameOffset + bytes.readUInt16BE(nameOffset + 4) + bytes.readUInt16BE(record + 10);
    const value = Buffer.from(bytes.subarray(start, start + bytes.readUInt16BE(record + 8)));
    const unicode = [0, 3].includes(bytes.readUInt16BE(record));
    if (unicode) value.swap16();
    notices.add(value.toString(unicode ? 'utf16le' : 'utf8'));
  }
  if (![...notices].some(n => n.includes('SIL Open Font License, Version 1.1'))) throw new Error(`Review changed font license: ${file}`);
  fontNotices.push(`${file}\nSHA-256: ${sha256(path.join(fontDir, file))}\n${[...notices].join('\n\n')}`);
}
const fontNoticePath = path.join(out, 'KaTeX-font-NOTICE.txt');
writeFileSync(fontNoticePath, `${fontNotices.join('\n\n')}\n`);
collect({ ecosystem: 'fonts', name: 'KaTeX-fonts', version: readJson(path.join(root, 'node_modules/katex/package.json')).version,
  license: 'OFL-1.1', source: npmLock.packages['node_modules/katex'].resolved, scope: 'unmodified bundled font files' },
fontDir, [fontNoticePath]);
const sysroot = capture('rustc', ['--print', 'sysroot']);
const rustDocs = path.join(sysroot, 'share/doc/rust');
const rustVersion = capture('rustc', ['--version']).split(' ')[1];
// rust-std is compiled into the binary; retain its complete upstream attribution.
if (!existsSync(path.join(rustDocs, 'COPYRIGHT-library.html'))) throw new Error('Rust library copyright material missing');
collect({ ecosystem: 'toolchain', name: 'rust-standard-library', version: rustVersion,
  license: 'MIT OR Apache-2.0; third-party notices in COPYRIGHT-library.html',
  source: `https://github.com/rust-lang/rust/tree/${rustVersion}/library`, scope: 'compiled runtime' },
  rustDocs, [path.join(rustDocs, 'COPYRIGHT-library.html')]);
collect({ ecosystem: 'template', name: 'tauri-nsis', version: '2.12.1', license: 'MIT',
  source: 'https://github.com/tauri-apps/tauri/tree/tauri-cli-v2.12.1/crates/tauri-bundler/src/bundle/windows/nsis',
  scope: 'installer; locally modified as documented in installer/README.md' }, path.join(root, 'src-tauri/installer'));
if (process.platform === 'win32') {
  collect({ ecosystem: 'installer', name: 'NSIS', version: '3.11', license: 'Zlib; LZMA module CPL-1.0; see COPYING',
    source: 'https://sourceforge.net/projects/nsis/files/NSIS%203/3.11/nsis-3.11-src.tar.bz2/download',
    scope: 'unmodified installer runtime and compression modules' });
  collect({ ecosystem: 'installer', name: 'nsis-tauri-utils', version: '0.5.3', license: 'MIT',
    source: 'https://github.com/tauri-apps/nsis-tauri-utils/tree/nsis_tauri_utils-v0.5.3', scope: 'unmodified installer plugin' });
}
if (missing.length) throw new Error(`Missing license evidence:\n${missing.join('\n')}`);
copyFileSync(path.join(root, 'LICENSE'), path.join(out, 'CueNote-LICENSE.txt'));
copyFileSync(path.join(root, 'package-lock.json'), path.join(out, 'package-lock.json'));
copyFileSync(path.join(root, 'src-tauri/Cargo.lock'), path.join(out, 'Cargo.lock'));
writeJson(path.join(out, 'components.json'), { format: 1, target: target(), components });
writeFileSync(path.join(out, 'THIRD-PARTY-NOTICES.txt'),
  'CueNote third-party attribution\n\n' +
  'components.json identifies exact versions, source archives, license expressions and original license/notice texts.\n' +
  'This inventory conservatively includes build/test tools as well as shipped dependencies; inclusion does not imply every package is in the executable.\n' +
  'Unmodified MPL-2.0 sources (including selectors/cssparser) are available at each recorded versioned source URL.\n' +
  'WebView2 (Windows) and WKWebView (macOS) are system prerequisites, not CueNote-owned source components.\n\n' +
  components.map(p => `${p.ecosystem}: ${p.name}@${p.version}\nLicense: ${p.license}\nSource: ${p.source}\n` +
    p.texts.map(t => `Text: ${t.file}`).join('\n')).join('\n\n') + '\n');
console.log(`Collected license texts for ${components.length} components (${target()})`);
