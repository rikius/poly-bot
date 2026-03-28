

# when running first time bot need to check if we have blocker on geoip

use polymarket_client_sdk::clob::Client;

let client = Client::default();
let geo = client.check_geoblock().await?;

if geo.blocked {
    println!("Trading not available in {}", geo.country);
} else {
    println!("Trading available");
}



# previous there were ability to have simulation 
now we are not able to have it. what's changed did we removed that functionality? we need to recover it.




# check that we will not exeed Rate Limits

link for rate limits
https://docs.polymarket.com/api-reference/rate-limits




# authentification

why we still use:

```
# Ethereum signing (PrivateKeySigner for SDK sign())
alloy = { version = "1", features = ["signers", "signer-local"] }
```


this is an example
```
use std::str::FromStr;
use polymarket_client_sdk::POLYGON;
use polymarket_client_sdk::auth::{LocalSigner, Signer};
use polymarket_client_sdk::clob::{Client, Config};

let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")?;
let signer = LocalSigner::from_str(&private_key)?
    .with_chain_id(Some(POLYGON));

// Creates new credentials or derives existing ones,
// then initializes the authenticated client — all in one step
let client = Client::new("https://clob.polymarket.com", Config::default())?
    .authentication_builder(&signer)
    .authenticate()
    .await?;

let credentials = client.credentials();
println!("API Key: {}", credentials.key());
```


