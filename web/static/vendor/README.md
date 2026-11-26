# xterm.js

The terminal loads these published ES modules and stylesheet directly from Deskway's static
files. The two packages use the MIT license.

| Package and registry archive | Archive path | Local file | Bytes | SHA-256 |
|---|---|---|---:|---|
| [@xterm/xterm 6.0.0](https://registry.npmjs.org/@xterm/xterm/-/xterm-6.0.0.tgz) | `package/lib/xterm.mjs` | `xterm.mjs` | 344970 | `b336ec65a086c056d4804b3d4c2347da5663d3f23c3f25be866467bd8857ad59` |
| [@xterm/xterm 6.0.0](https://registry.npmjs.org/@xterm/xterm/-/xterm-6.0.0.tgz) | `package/css/xterm.css` | `xterm.css` | 7112 | `854a7c0fb70e8b1a083c16797ab827299fb18744f5ad34f227b48337e33293c6` |
| [@xterm/xterm 6.0.0](https://registry.npmjs.org/@xterm/xterm/-/xterm-6.0.0.tgz) | `package/LICENSE` | `xterm.LICENSE` | 1261 | `b569f629d00f2626a8100df2a1798210535621e42164dfd426a6fe5aac7b0ccd` |
| [@xterm/addon-fit 0.11.0](https://registry.npmjs.org/@xterm/addon-fit/-/addon-fit-0.11.0.tgz) | `package/lib/addon-fit.mjs` | `addon-fit.mjs` | 1967 | `2d87e1bddc73be9111de8beee5370c3bb7aac9c94e18e6f245f02ca741ef1769` |
| [@xterm/addon-fit 0.11.0](https://registry.npmjs.org/@xterm/addon-fit/-/addon-fit-0.11.0.tgz) | `package/LICENSE` | `addon-fit.LICENSE` | 1103 | `e256f01188af527e4d06d21d06fbf785ae9c50d4b328bf03cbe0ba7f0aa4228f` |

To update, fetch the version's metadata from
`https://registry.npmjs.org/@xterm%2f<package>/<version>`. Download `dist.tarball` and verify its
SHA-512 in base64 against `dist.integrity` (for example, with `openssl dgst -sha512 -binary`
and `openssl base64 -A`). Use `tar -xOf` to copy the five archive members above as published.
Update the versions, sizes and SHA-256 values in this table. Import both modules directly and
run the browser terminal trial.
