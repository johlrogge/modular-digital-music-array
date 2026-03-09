Feature: Parse date expression
  Parse date expression given today's date and a date expression.

  Scenario Outline: Year expressions
    Given today's date is "<today>"
    And the date expression is "<expression>"
    When I parse the date expression
    Then the result should be "<result>"

    Examples:
      | today      | expression | result     |
      | 2024-01-01 | ~/2/1      | 2024-02-01 |
      | 2024-01-01 | +1/2/1     | 2025-02-01 |
      | 2024-01-01 | +5/2/1     | 2029-02-01 |
      | 2024-01-01 | -1/2/1     | 2023-02-01 |
      | 2024-01-01 | -5/2/1     | 2019-02-01 |
      | 2024-01-01 | $/2/1      | error      |
      | 2024-01-01 | ^/2/1      | error      |

  Scenario Outline: Month expressions
    Given today's date is "<today>"
    And the date expression is "<expression>"
    When I parse the date expression
    Then the result should be "<result>"

    Examples:
      | today      | expression | result     |
      | 2024-02-01 | ~/~/2      | 2024-02-02 |
      | 2024-02-01 | ~/+1/2     | 2024-03-02 |
      | 2024-02-01 | ~/+5/2     | 2024-07-02 |
      | 2024-02-01 | ~/-1/2     | 2024-01-02 |
      | 2024-02-01 | ~/-2/2     | 2023-12-02 |
      | 2024-02-01 | -3/2       | 2023-11-02 |
      | 2024-02-01 | ~/+1/2     | 2024-03-02 |
      | 2024-02-01 | ~/+2/2     | 2024-04-02 |
      | 2024-12-01 | +2/2       | 2025-02-02 |
      | 2024-01-01 | 2024/$/1   | 2024-12-01 |
      | 2024-11-01 | 2024/^/1   | 2024-01-01 |
      | 2024-01-01 | $/1        | 2024-12-01 |
      | 2024-11-01 | ^/1        | 2024-01-01 |

  Scenario Outline: Day expressions
    Given today's date is "<today>"
    And the date expression is "<expression>"
    When I parse the date expression
    Then the result should be "<result>"

    Examples:
      | today      | expression | result     |
      | 2024-11-01 | ~/~        | 2024-11-01 |
      | 2024-11-01 | ~          | 2024-11-01 |
      | 2024-11-01 | ~/2        | 2024-11-02 |
      | 2024-11-01 | ~/+2       | 2024-11-03 |
      | 2024-11-01 | -2         | 2024-10-30 |
      | 2024-11-12 | ^          | 2024-11-01 |
      | 2024-11-12 | $          | 2024-11-30 |
      | 2024-02-05 | $          | 2024-02-29 |
      | 2025-02-05 | $          | 2025-02-28 |
