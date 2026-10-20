import project

project.Project {
    project_id = "wamn-dev"
    zone = "us-central1-a"
    cluster_name = "wamn"
    release_channel = "regular"
    network = "wamn"
    subnetwork = "wamn-us-central1"
    pod_secondary_range_name = "pods"
    service_secondary_range_name = "services"
    node_service_account = "wamn-nodes@wamn-dev.iam.gserviceaccount.com"
    node_pools = [
        {
            name = "main"
            machine_type = "e2-standard-2"
            spot = True
            disk_type = "pd-standard"
            disk_size = 30
            size = 1
            label = ""
            taint = ""
        }
        {
            name = "bench"
            machine_type = "e2-standard-2"
            spot = True
            disk_type = "pd-standard"
            disk_size = 30
            size = 1
            label = "role=bench-host"
            taint = "role=bench-host:NoSchedule"
        }
        {
            name = "pg"
            machine_type = "e2-standard-2"
            spot = False
            disk_type = "pd-standard"
            disk_size = 50
            size = 1
            label = "role=postgres"
            taint = "role=postgres:NoSchedule"
        }
    ]
}
