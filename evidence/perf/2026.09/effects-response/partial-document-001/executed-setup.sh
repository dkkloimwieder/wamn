  partial_journey_document=$work_dir/journey-partial.json
  write_journey_document journey_spec "$journey_schema" "$partial_journey_document"
  amend_journey_document "$journey_schema" "$partial_journey_document" runtime \
    "$(jq -n --arg endpoint "http://$runtime_node_ip:$runtime_node_port" \
        --arg pallet "$app_fixture_pallet_id" --arg to "$app_fixture_location_a_id" \
        '{route_endpoint: $endpoint, pallet_id: $pallet, to_location_id: $to}')"
