#!/usr/bin/env python3
"""Validate architecture sources and embed offline map data into the HTML guide."""
import argparse
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / 'docs/architecture/architecture-map.json'
HTML = ROOT / 'docs/architecture-current.html'


def render(check=False):
    data = json.loads(DATA.read_text(encoding="utf-8"))
    modules = {m['id']: m for m in data['modules']}
    assert len(modules) == len(data['modules']), 'duplicate module IDs'
    view_ids = {v['id'] for v in data['views']}
    assert len(view_ids) == len(data['views']), 'duplicate view IDs'
    source = HTML.read_text(encoding="utf-8")
    for module in modules.values():
        assert f'id="{module["section"]}"' in source, module['section']
        assert not module.get('drill') or module['drill'] in view_ids
        for path in [module['path'], *(p['path'] for p in module['parts'])]:
            assert (ROOT / path).is_file(), f'missing source: {path}'
    visible = set()
    for view in data['views']:
        ids = {node['id'] for node in view['nodes']}
        assert len(ids) == len(view['nodes']) and ids <= modules.keys(), view['id']
        visible |= ids
        for edge in view['edges']:
            assert edge['source'] in ids and edge['target'] in ids, edge
            assert edge['kind'] in {'control', 'data', 'store', 'dependency'}, edge
    assert visible == modules.keys(), 'unreachable module'
    # Validate static and existing flow-source links too, after source moves.
    for link in re.findall(r'[\"\'](\.\./(?:src|scripts|packaging)/[^\"\']+)[\"\']', source):
        assert (HTML.parent / link).exists(), f'broken guide link: {link}'
    payload = json.dumps(data, ensure_ascii=False, indent=2).replace('<', '\\u003c').replace('>', '\\u003e').replace('&', '\\u0026')
    output, count = re.subn(r'(<script id="architecture-map-data" type="application/json">).*?(</script>)', lambda m: m[1]+'\n'+payload+'\n'+m[2], source, flags=re.S)
    assert count == 1, 'expected one map data block'
    if check:
        assert source == output, 'map data stale; run python3 scripts/update-architecture-map.py'
    else:
        HTML.write_text(output, encoding="utf-8")
    print(f'Architecture map OK: {len(modules)} modules, {len(data["views"])} views; source paths verified.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    render(parser.parse_args().check)
