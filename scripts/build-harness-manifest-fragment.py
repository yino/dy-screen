#!/usr/bin/env python3
"""生成 Harness 组件在 RuntimeManifest（runtime-manifest.json）里的片段。

把 harness.runtime（Node 二进制）/ harness.profile / harness.skill 三个组件，
连同 Node MIT 许可证，翻译成 RuntimeManifest 的 components + licenses 条目。

用法：
    python3 scripts/build-harness-manifest-fragment.py <platform>
    platform ∈ {darwin-arm64, darwin-x64, win-x64}

前置：先跑 scripts/build-harness-runtime.sh/.ps1 解压对应平台的 Node 运行时。

产物：stdout 输出 JSON 片段（components + licenses）。构建脚本把它并入每平台
runtime-manifest.json 的 components / licenses，archive 的 size_bytes / sha256
需按含 Node 二进制的新 bundle 重新计算，signature 由发布流程施加。
"""
import hashlib
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
LOCK = json.loads(
    (ROOT / "resources/harness-source/runtime/node-lock.json").read_text()
)
VERSION = LOCK["version"]

# 各平台 Node 二进制在解压产物里的相对路径（相对仓库根）。
NODE_BINARY = {
    "darwin-arm64": "resources/harness-source/runtime/darwin-arm64/bin/node",
    "darwin-x64": "resources/harness-source/runtime/darwin-x64/bin/node",
    "win-x64": f"resources/harness-source/runtime/win-x64/node-v{VERSION}-win-x64/node.exe",
}


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    digest.update(path.read_bytes())
    return digest.hexdigest()


def file_entry(rel: str) -> dict:
    path = ROOT / rel
    return {
        "path": rel,
        "sizeBytes": path.stat().st_size,
        "sha256": sha256(path),
        "executable": "node" in rel or rel.endswith("node.exe"),
    }


def main() -> int:
    if len(sys.argv) != 2:
        print("用法：build-harness-manifest-fragment.py <platform>", file=sys.stderr)
        return 2
    platform = sys.argv[1]
    if platform not in NODE_BINARY:
        print(f"未知平台：{platform}", file=sys.stderr)
        return 2

    node_rel = NODE_BINARY[platform]
    if not (ROOT / node_rel).is_file():
        print(f"缺少 Node 二进制：{node_rel}（先跑 build-harness-runtime）", file=sys.stderr)
        return 2

    profile_rel = "resources/harness-source/profile.json"
    skill_files = sorted(
        str(p.relative_to(ROOT))
        for p in (ROOT / "resources/harness-source/skills").glob("*.skill.json")
    )

    fragment = {
        "components": [
            {
                "id": "harness.runtime",
                "version": VERSION,
                "required": False,
                "capability": "enhancement",
                "scripts": [],
                "files": [file_entry(node_rel)],
            },
            {
                "id": "harness.profile",
                "version": VERSION,
                "required": False,
                "capability": "enhancement",
                "scripts": [],
                "files": [file_entry(profile_rel)],
            },
            {
                "id": "harness.skill",
                "version": VERSION,
                "required": False,
                "capability": "enhancement",
                "scripts": [],
                "files": [file_entry(rel) for rel in skill_files],
            },
        ],
        "licenses": [
            {
                "id": "nodejs",
                "path": "licenses/Node-MIT.txt",
                "license": "MIT",
                "source": "https://nodejs.org/",
            }
        ],
    }
    json.dump(fragment, sys.stdout, ensure_ascii=False, indent=2)
    print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
