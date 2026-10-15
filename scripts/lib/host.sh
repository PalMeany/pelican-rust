# Sourced by the scripts that drive Docker. On macOS, Docker lives in the lima VM
# "pelican" (PELICAN_VM); on a Linux machine the scripts use the local Docker.
#   on_host <cmd...>      run a command where Docker is (the VM keeps the current directory)
#   PELICAN_CPUSET=24-31  on a shared server: pin the build, test and bench containers to
#                         these CPUs with a low CPU weight, leaving the rest of the machine alone
if command -v limactl >/dev/null 2>&1; then
  on_host() { limactl shell --workdir "$PWD" "${PELICAN_VM:-pelican}" -- "$@"; }
else
  on_host() { "$@"; }
fi
if [[ -n "${PELICAN_CPUSET:-}" ]]; then
  export PELICAN_CPU_SHARES="${PELICAN_CPU_SHARES:-256}"
fi
# `docker run` options for PELICAN_CPUSET.
cpu_opts() {
  [[ -n "${PELICAN_CPUSET:-}" ]] && echo "--cpuset-cpus ${PELICAN_CPUSET} --cpu-shares ${PELICAN_CPU_SHARES}"
  return 0
}
