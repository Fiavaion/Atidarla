use super::*;

const SHA_A: &str = "40ec400881e27d1376c7c95c5bd495f407b33756e80eb6365e301c33a07af6e5";
const SHA_B: &str = "db169d69145ec4ff064e49d99c95fa05d3eb04ee453de35824a6d0f325513549";

fn pin(class: LicenceClass, sha: &str) -> ModelPin {
    ModelPin {
        licence_class: class,
        sha256: sha.into(),
    }
}

fn with_models() -> Project {
    let mut p = Project::new("AI show");
    p.ai.commercial_safe_only = true;
    p.models
        .insert("taesd".into(), pin(LicenceClass::CommercialSafe, SHA_B));
    p.models
        .insert("sd-turbo".into(), pin(LicenceClass::Restricted, SHA_A));
    p.models.insert(
        "x.nc_model-2".into(),
        pin(LicenceClass::NonCommercial, SHA_A),
    );
    p
}

#[test]
fn ai_settings_and_models_round_trip_in_id_order() {
    let p = with_models();
    let text = project_to_string(&p);
    let back = project_from_str(&text).unwrap();
    assert_eq!(back.ai, p.ai);
    assert_eq!(back.models, p.models);
    assert_eq!(project_to_string(&back), text, "byte-stable");
    // Written in ID order whatever order they were added in.
    let ids: Vec<usize> = ["sd-turbo", "taesd", "x.nc_model-2"]
        .iter()
        .map(|id| text.find(&format!("id = \"{id}\"")).unwrap())
        .collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "{text}");
    assert!(
        text.contains("[ai]\ncommercial_safe_only = true\n"),
        "{text}"
    );
    assert!(
        text.contains("licence_class = \"non-commercial\""),
        "{text}"
    );
}

#[test]
fn every_combination_round_trips() {
    let ids = ["a", "b.c", "sd-turbo", "z_9"];
    for mask in 0u32..1 << (ids.len() + 1) {
        let mut p = Project::new("p");
        p.ai.commercial_safe_only = mask & 1 == 1;
        for (i, id) in ids.iter().enumerate() {
            if mask >> (i + 1) & 1 == 1 {
                let class = LicenceClass::ALL[i % 3];
                p.models.insert((*id).into(), pin(class, SHA_A));
            }
        }
        let text = project_to_string(&p);
        let back = project_from_str(&text).unwrap();
        assert_eq!((back.ai, back.models), (p.ai.clone(), p.models.clone()));
        assert_eq!(project_to_string(&project_from_str(&text).unwrap()), text);
    }
}

#[test]
fn projects_without_ai_keep_their_bytes() {
    let p = Project::new("Plain");
    let text = project_to_string(&p);
    assert!(!text.contains("[ai]") && !text.contains("models"), "{text}");
    let back = project_from_str(&text).unwrap();
    assert_eq!(back.ai, AiSettings::default());
    assert!(back.models.is_empty());
    // An explicit default is accepted too.
    let explicit = format!("{text}\n[ai]\ncommercial_safe_only = false\n");
    assert_eq!(
        project_to_string(&project_from_str(&explicit).unwrap()),
        text
    );
}

#[test]
fn inline_model_tables_are_accepted() {
    let text = format!(
        "format = 1\nname = \"p\"\nmodels = [{{ id = \"taesd\", licence_class = \"commercial-safe\", sha256 = \"{SHA_B}\" }}]\n"
    );
    let p = project_from_str(&text).unwrap();
    assert_eq!(p.models["taesd"], pin(LicenceClass::CommercialSafe, SHA_B));
}

#[test]
fn manifest_errors_say_what_and_where() {
    let err = |extra: &str| {
        project_from_str(&format!("format = 1\nname = \"p\"\n{extra}"))
            .unwrap_err()
            .to_string()
    };
    let model = |id: &str, class: &str, sha: &str| {
        format!("[[models]]\nid = \"{id}\"\nlicence_class = \"{class}\"\nsha256 = \"{sha}\"\n")
    };
    let e = err(&model("m", "free", SHA_A));
    assert!(e.contains("model 1 (m): `licence_class` must be"), "{e}");
    let e = err(&model("m", "restricted", "ABC"));
    assert!(
        e.contains("`sha256` must be 64 lower-case hex digits"),
        "{e}"
    );
    let e = err(&model("Bad/Id", "restricted", SHA_A));
    assert!(e.contains("model 1: `id` `Bad/Id`"), "{e}");
    let twice = format!(
        "{}{}",
        model("m", "restricted", SHA_A),
        model("m", "restricted", SHA_A)
    );
    assert!(err(&twice).contains("model 2: model `m` is listed twice"));
    let e = err("[ai]\ncommercial_safe_only = 1\n");
    assert!(e.contains("must be true or false"), "{e}");
    let e = err("[ai]\nsovreign = true\n");
    assert!(e.contains("[ai]: unknown key `sovreign`"), "{e}");
    let e = err(&format!(
        "[[models]]\nid = \"m\"\nlicence_class = \"restricted\"\nsha256 = \"{SHA_A}\"\npath = \"x\"\n"
    ));
    assert!(e.contains("model 1: unknown key `path`"), "{e}");
}

#[test]
fn model_ids_are_portable_folder_names() {
    for ok in ["sd-turbo", "taesd", "depth-anything-v2-small", "a.b_c"] {
        assert!(models::valid_model_id(ok), "{ok}");
    }
    for bad in [
        "",
        ".",
        "..",
        ".hidden",
        "a/b",
        "a\\b",
        "SD",
        "a b",
        &"x".repeat(65),
    ] {
        assert!(!models::valid_model_id(bad), "{bad}");
    }
}

#[test]
fn the_manifest_survives_a_folder_save_and_load() {
    let dir = std::env::temp_dir().join(format!("lsr-format-models-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let project = with_models();
    save(&dir, &project).unwrap();
    let loaded = load(&dir).unwrap();
    assert_eq!(loaded, project);
    let first = std::fs::read(dir.join("project.toml")).unwrap();
    save(&dir, &loaded).unwrap();
    assert_eq!(std::fs::read(dir.join("project.toml")).unwrap(), first);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn with_hosts() -> Project {
    let mut p = Project::new("Remote show");
    p.ai.sovereign = false;
    p.ai.spend_cap_usd = 12.5;
    p.ai.hosts
        .insert("lan:studio".into(), "192.168.1.20:7420".into());
    p.ai.hosts
        .insert("cloud:runpod".into(), "pod.example.net:7420".into());
    p
}

#[test]
fn sovereign_spend_cap_and_hosts_round_trip_in_name_order() {
    let p = with_hosts();
    let text = project_to_string(&p);
    let back = project_from_str(&text).unwrap();
    assert_eq!(back.ai, p.ai);
    assert_eq!(project_to_string(&back), text, "byte-stable");
    assert!(
        text.contains("[ai]\nsovereign = false\nspend_cap_usd = 12.5\n"),
        "{text}"
    );
    let cloud = text.find("name = \"cloud:runpod\"").unwrap();
    let lan = text.find("name = \"lan:studio\"").unwrap();
    assert!(cloud < lan, "hosts in name order: {text}");
    assert!(text.contains("[[ai.hosts]]"), "{text}");
    assert!(!text.contains("token"), "{text}");
}

#[test]
fn sovereign_is_on_by_default_and_written_only_when_off() {
    assert!(Project::new("p").ai.sovereign);
    // A project from before M4 has no key: Sovereign mode is on.
    let old = project_from_str("format = 1\nname = \"p\"\n").unwrap();
    assert!(old.ai.sovereign);
    assert_eq!(old.ai.spend_cap_usd, 0.0);
    let explicit = project_from_str("format = 1\nname = \"p\"\n[ai]\nsovereign = true\n").unwrap();
    assert_eq!(explicit.ai, AiSettings::default());
    assert!(!project_to_string(&explicit).contains("[ai]"));
    // An integer cap is read as dollars.
    let int = project_from_str("format = 1\nname = \"p\"\n[ai]\nspend_cap_usd = 3\n").unwrap();
    assert_eq!(int.ai.spend_cap_usd, 3.0);
}

#[test]
fn hosts_alone_write_no_empty_ai_header() {
    let mut p = Project::new("p");
    p.ai.hosts.insert("lan:a".into(), "10.0.0.2:7420".into());
    let text = project_to_string(&p);
    assert!(!text.contains("[ai]\n"), "{text}");
    assert!(
        text.contains("[[ai.hosts]]\nname = \"lan:a\"\naddr = \"10.0.0.2:7420\"\n"),
        "{text}"
    );
    let back = project_from_str(&text).unwrap();
    assert_eq!(back.ai, p.ai);
    assert_eq!(project_to_string(&back), text);
}

#[test]
fn every_ai_settings_combination_round_trips() {
    for mask in 0u32..32 {
        let mut p = Project::new("p");
        p.ai.commercial_safe_only = mask & 1 != 0;
        p.ai.sovereign = mask & 2 != 0;
        p.ai.spend_cap_usd = if mask & 4 != 0 { 0.75 } else { 0.0 };
        if mask & 8 != 0 {
            p.ai.hosts.insert("lan:z".into(), "[fd00::2]:7420".into());
        }
        if mask & 16 != 0 {
            p.ai.hosts
                .insert("cloud:decart".into(), "api.example.com:443".into());
        }
        let text = project_to_string(&p);
        let back = project_from_str(&text).unwrap();
        assert_eq!(back.ai, p.ai, "{text}");
        assert_eq!(project_to_string(&back), text);
    }
}

#[test]
fn ai_setting_errors_say_what_and_where() {
    let err = |extra: &str| {
        project_from_str(&format!("format = 1\nname = \"p\"\n{extra}"))
            .unwrap_err()
            .to_string()
    };
    let e = err("[ai]\nsovereign = \"yes\"\n");
    assert!(e.contains("`sovereign` must be true or false"), "{e}");
    let e = err("[ai]\nspend_cap_usd = -1.0\n");
    assert!(e.contains("`spend_cap_usd` must be a number"), "{e}");
    let host =
        |name: &str, addr: &str| format!("[[ai.hosts]]\nname = \"{name}\"\naddr = \"{addr}\"\n");
    let e = err(&host("studio", "10.0.0.2:7420"));
    assert!(
        e.contains("[ai] host 1: `name` `studio` must be lan:<name>"),
        "{e}"
    );
    let e = err(&host("lan:Studio", "10.0.0.2:7420"));
    assert!(e.contains("must be lan:<name>"), "{e}");
    let e = err(&host("lan:studio", "10.0.0.2"));
    assert!(e.contains("[ai] host 1 (lan:studio): `addr`"), "{e}");
    let twice = format!(
        "{}{}",
        host("lan:a", "10.0.0.2:7420"),
        host("lan:a", "10.0.0.3:7420")
    );
    assert!(err(&twice).contains("[ai] host 2: host `lan:a` is listed twice"));
    // A token is refused: it belongs in the per-user store.
    let e = err("[[ai.hosts]]\nname = \"lan:a\"\naddr = \"10.0.0.2:7420\"\ntoken = \"s3cret\"\n");
    assert!(e.contains("[ai] host 1: unknown key `token`"), "{e}");
}
