#!/usr/bin/env bash
# Attach the persistent volume without detaching another instance's storage.
set -euo pipefail

region="$1"
volume_id="$2"
instance_id="$3"
group_name="$4"
hook_name="$5"
wait_seconds="$6"

heartbeat() {
  aws autoscaling record-lifecycle-action-heartbeat --region "$region" \
    --lifecycle-hook-name "$hook_name" --auto-scaling-group-name "$group_name" \
    --instance-id "$instance_id"
}

abandon() {
  aws autoscaling complete-lifecycle-action --region "$region" \
    --lifecycle-hook-name "$hook_name" --auto-scaling-group-name "$group_name" \
    --instance-id "$instance_id" --lifecycle-action-result ABANDON
  exit 1
}

[[ "$wait_seconds" =~ ^[1-9][0-9]*$ ]] || exit 2
export AWS_MAX_ATTEMPTS=2
export AWS_PAGER=""

aws() {
  command aws --cli-connect-timeout 5 --cli-read-timeout 10 "$@"
}

trap 'echo "persistent volume handoff failed" >&2; abandon' ERR
deadline=$((SECONDS + wait_seconds))
attach_requested=false
while (( SECONDS < deadline )); do
  heartbeat
  description="$(aws ec2 describe-volumes --region "$region" \
    --volume-ids "$volume_id" --output json)"
  if jq -e --arg instance "$instance_id" '
    .Volumes | length == 1' <<<"$description" >/dev/null &&
    jq -e --arg instance "$instance_id" '
      .Volumes[0] | .State == "in-use" and (.Attachments | length == 1)
      and .Attachments[0].InstanceId == $instance and .Attachments[0].State == "attached"
    ' <<<"$description" >/dev/null; then
    exit 0
  fi
  if [ "$attach_requested" = false ] && jq -e '
    .Volumes[0] | .State == "available" and (.Attachments | length == 0)
  ' <<<"$description" >/dev/null; then
    aws ec2 attach-volume --region "$region" --volume-id "$volume_id" \
      --instance-id "$instance_id" --device /dev/sdf >/dev/null
    attach_requested=true
  fi
  remaining=$((deadline - SECONDS))
  (( remaining > 0 )) || break
  sleep "$((remaining < 5 ? remaining : 5))"
done
echo "persistent volume handoff timed out" >&2
abandon
