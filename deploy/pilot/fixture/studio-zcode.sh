#!/bin/sh
set -eu
if [ "$1" = app-server ]; then
  IFS= read -r request
  printf '%s\n' '{"id":1,"result":{"settings":{"model":{"available":[{"ref":{"providerId":"team","modelId":"qa"},"providerLabel":"Team","label":"QA","reasoning":{"levels":[{"value":"low","label":"Low"},{"value":"high","label":"High"}],"defaultLevel":"high"}},{"ref":{"providerId":"team","modelId":"fast"},"label":"Fast"},{"ref":{"providerId":"other","modelId":"solo"},"label":"Solo"}]}}}}'
  exit 0
fi
mode=$2
while [ "$1" != --attach ]; do shift; done
grep -q 'Return the exact widened value.' "$2"
case "$mode" in
  edit)
    grep -q 'u64::from(seconds) \* 1_000' normalize.rs
    printf 'pub fn milliseconds(seconds: u32) -> u64 { u64::from(seconds) * 1000 }\n' > normalize.rs
    printf '%s\n' 'AEP-RELAY: {"note":"ZCode applied the revised decision.","question":""}'
    ;;
  plan)
    grep -q 'ZCode applied the revised decision.' "$2"
    printf '%s\n' 'AEP-RELAY: {"note":"Reviewed the change.","question":"","report":"Reviewed the conversion and revised decision.","blocking_findings":[]}'
    ;;
  *) exit 1 ;;
esac
