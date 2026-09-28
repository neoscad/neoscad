#!/bin/sh
# Install CadQuery 2.8.0 into a project-local venv for the agent CAD
# comparison (docs/agent-eval.md, "CAD comparison"). Never global, never
# sudo: the venv lives under the gitignored .cache/, and deleting it undoes
# the install. ModelRift ran CadQuery 2.8.0 on Python 3.14, so we do too.
#
#   scripts/agent-eval/cad/setup-cadquery.sh [VENV_DIR]
set -eu
root=$(cd "$(dirname "$0")/../../.." && pwd)
venv=${1:-"$root/.cache/agent-eval/cadquery-venv"}
if ! command -v uv >/dev/null 2>&1; then
    echo "needs uv (https://docs.astral.sh/uv/)" >&2
    exit 1
fi
uv venv --python 3.14 "$venv"
uv pip install --python "$venv/bin/python" 'cadquery==2.8.0'
"$venv/bin/python" -c 'import cadquery, sys; print("cadquery", cadquery.__version__, "python", sys.version.split()[0])'
du -sh "$venv"
