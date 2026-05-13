//! Drink catalog shared between systems that take orders (customers) and
//! systems that present a brew menu (coffee machine UI).

#[derive(Debug, Clone, Copy)]
pub struct Drink {
    pub name: &'static str,
    pub brew_secs: f32,
    pub price: i64,
}

pub const DRINKS: &[Drink] = &[
    Drink {
        name: "Coffee",
        brew_secs: 4.0,
        price: 3,
    },
    Drink {
        name: "Latte",
        brew_secs: 6.0,
        price: 5,
    },
    Drink {
        name: "Tea",
        brew_secs: 3.0,
        price: 2,
    },
    Drink {
        name: "Espresso",
        brew_secs: 3.0,
        price: 3,
    },
    Drink {
        name: "Cappuccino",
        brew_secs: 6.0,
        price: 5,
    },
    Drink {
        name: "Hot Chocolate",
        brew_secs: 5.0,
        price: 4,
    },
];

pub fn price_for(name: &str) -> Option<i64> {
    DRINKS.iter().find(|d| d.name == name).map(|d| d.price)
}
