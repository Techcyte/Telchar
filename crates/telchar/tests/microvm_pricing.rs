//! Published MicroVM prices select exact ARM regional rates and round upward.

use telchar::microvm::pricing::ComputePrice;

fn catalog() -> serde_json::Value {
    serde_json::json!({
        "offerCode": "AWSLambda", "version": "20261022183959",
        "products": {
            "cpu": {"attributes": {"servicecode":"AWSLambda", "regionCode":"us-west-2", "group":"AWS-Lambda-MicroVM-vCPU-Second-ARM"}},
            "memory": {"attributes": {"servicecode":"AWSLambda", "regionCode":"us-west-2", "group":"AWS-Lambda-MicroVM-Memory-GB-Second-ARM"}}
        },
        "terms": {"OnDemand": {
            "cpu": {"cpu.term": {"priceDimensions": {"cpu.rate": {"unit":"vCPU-Seconds", "beginRange":"0", "endRange":"Inf", "appliesTo":[], "pricePerUnit":{"USD":"0.0000276944"}}}}},
            "memory": {"memory.term": {"priceDimensions": {"memory.rate": {"unit":"GB-Seconds", "beginRange":"0", "endRange":"Inf", "appliesTo":[], "pricePerUnit":{"USD":"0.0000036667"}}}}}
        }}
    })
}

#[test]
fn peak_compute_reservation_uses_published_rates_without_discounts() {
    let price = ComputePrice::from_catalog(&catalog(), "us-west-2", 2048).unwrap();
    assert_eq!(price.picodollars_per_second(), 140_111_200);
    assert!(price.reference().contains("20261022183959"));
    assert_eq!(
        ComputePrice::from_catalog(&catalog(), "us-west-2", 8192)
            .unwrap()
            .picodollars_per_second(),
        560_444_800
    );
}

#[test]
fn missing_ambiguous_or_unsupported_prices_fail_closed() {
    assert!(ComputePrice::from_catalog(&catalog(), "eu-west-1", 2048).is_err());
    assert!(ComputePrice::from_catalog(&catalog(), "us-west-2", 16384).is_err());
    let mut missing = catalog();
    missing["terms"]["OnDemand"]
        .as_object_mut()
        .unwrap()
        .remove("memory");
    assert!(ComputePrice::from_catalog(&missing, "us-west-2", 2048).is_err());
    let mut duplicate = catalog();
    duplicate["products"]["cpu-copy"] = duplicate["products"]["cpu"].clone();
    assert!(ComputePrice::from_catalog(&duplicate, "us-west-2", 2048).is_err());
    for invalid in ["-0.01", "0", "NaN", "1e-5"] {
        let mut value = catalog();
        value["terms"]["OnDemand"]["cpu"]["cpu.term"]["priceDimensions"]["cpu.rate"]["pricePerUnit"]
            ["USD"] = invalid.into();
        assert!(ComputePrice::from_catalog(&value, "us-west-2", 2048).is_err());
    }
}

#[test]
fn fractional_picodollars_round_up() {
    let mut value = catalog();
    value["terms"]["OnDemand"]["cpu"]["cpu.term"]["priceDimensions"]["cpu.rate"]["pricePerUnit"]
        ["USD"] = "0.0000276944001".into();
    assert_eq!(
        ComputePrice::from_catalog(&value, "us-west-2", 2048)
            .unwrap()
            .picodollars_per_second(),
        140_111_204
    );
}
