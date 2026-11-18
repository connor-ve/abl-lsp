# Packages from git

A small project that uses [abl-collection](https://github.com/connor-ve/abl-collection), an ABL library, as a package.

```
cd samples/packages
abl i                       # installs what abl.toml and abl.lock list into vendor/
abl                         # the library's src is now on the PROPATH
abl deps src/collections-demo.p
```

`abl.toml` names the package under `[deps]`, and `abl.lock` pins the exact commit, so every machine gets the same code. `vendor/` is not committed; `abl i` recreates it. To add a package of your own: `abl i github.com/owner/repo` (or a URL, or a local folder), optionally `@tag`.
