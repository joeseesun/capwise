# 灵录官网

> L2 | Parent: ../AGENTS.md

`index.html` 是静态官网首页。配乐库入口指向独立部署的 `/music/`。
`public/` 存放静态资源、离线页和搜索引擎入口。
`scripts/build.mjs` 校验首页资源，生成带版本的静态站点。
`deploy/` 存放官网和独立配乐库的 Nginx 配置。
`music/` 由批量上架工具生成共用目录、试听资源和独立曲目页。

[PROTOCOL]: Update this header on change, then check AGENTS.md
