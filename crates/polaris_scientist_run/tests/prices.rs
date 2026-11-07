//! Spend is priced from the built-in table: no network, no price file.
//!
//! The environment is process-wide, so this is a test file of its own with one
//! test.

mod support;

use polaris::models::pricing::PriceStore;
use polaris_code::usage::TokenMeter;
use polaris_cosmos::flow::{StopCause, TaskRunner as _};
use polaris_scientist_run::runner::DEFAULT_MODEL;

use support::{Fixture, MODEL_ID, fixture, request, text};

fn dollars(model: &str, add: impl FnOnce(&TokenMeter)) -> f64 {
    let meter = TokenMeter::new().with_pricing_store(PriceStore::builtin());
    add(&meter);
    meter
        .snapshot(model)
        .estimated_cost_usd
        .unwrap_or_else(|| panic!("{model} has a price"))
}

fn near(left: f64, right: f64) -> bool {
    (left - right).abs() < 1e-9
}

#[test]
fn sonnet_5_5_is_priced_from_the_built_in_table_and_no_price_file_is_written() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: nothing else runs yet; the runtime is built after these are set.
    unsafe {
        for name in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
        ] {
            std::env::set_var(name, home.path());
        }
    }

    // Per million tokens: $2 in, $10 out, $0.20 cache read, $2.50 cache write.
    for model in [MODEL_ID, DEFAULT_MODEL] {
        assert!(
            near(dollars(model, |m| m.add(1_000_000, 0)), 2.0),
            "{model}"
        );
        assert!(
            near(dollars(model, |m| m.add(0, 1_000_000)), 10.0),
            "{model}"
        );
        assert!(
            near(dollars(model, |m| m.add_cache(1_000_000, 0)), 0.2),
            "{model}"
        );
        assert!(
            near(dollars(model, |m| m.add_cache(0, 1_000_000)), 2.5),
            "{model}"
        );
    }

    // A whole call is priced the same way, and writes nothing under the
    // directories a price cache would use.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let fx = fixture().await;
            let folder = fx.folder("priced");
            fx.provider
                .script(&Fixture::key(&folder), vec![text("priced")]);
            let report = fx
                .runner
                .run(request("priced", &folder))
                .await
                .expect("a report");
            assert_eq!(report.stop, StopCause::Finished);
            assert_eq!(report.usage.micros, 100 * 2 + 10 * 10);
        });
    let left: Vec<_> = std::fs::read_dir(home.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(left.is_empty(), "files were written: {left:?}");
}
