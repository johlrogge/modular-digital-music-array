use crate::world::MdmaWorld;
use cucumber::{given, then, when};

#[given(expr = "today's date is {string}")]
async fn given_today(world: &mut MdmaWorld, today: String) {
    world.today = Some(today);
}

#[given(expr = "the date expression is {string}")]
async fn given_expression(world: &mut MdmaWorld, expression: String) {
    world.date_expression = Some(expression);
}

#[when("I parse the date expression")]
async fn when_parse(world: &mut MdmaWorld) {
    let today_str = world.today.as_ref().expect("today not set");
    let expression = world.date_expression.as_ref().expect("expression not set");

    let today =
        chrono::NaiveDate::parse_from_str(today_str, "%Y-%m-%d").expect("invalid today date");

    world.date_result = Some(
        date_expression::resolve(expression, today)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|_| "error".to_string()),
    );
}

#[then(expr = "the result should be {string}")]
async fn then_result(world: &mut MdmaWorld, expected: String) {
    let actual = world.date_result.as_ref().expect("no result");
    assert_eq!(actual, &expected, "date expression mismatch");
}
