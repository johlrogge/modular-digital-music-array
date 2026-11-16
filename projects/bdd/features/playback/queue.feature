Feature: Queue management

  # Playback takes only full hashes. The steps name a track by a prefix and
  # queue the full hash the library returns for it.
  Background:
    Given the library contains:
      | hash | artist  | title |
      | aabb | Artist1 | One   |
      | ccdd | Artist2 | Two   |

  Scenario: Append and play from queue
    Given the queue contains "aabb"
    When I append "ccdd" to the queue
    And I play from queue
    Then the now playing track should be "aabb"
    And the queue should contain 1 track

  Scenario: Playback refuses a short hash
    When I append the short hash "aabb" to the queue
    Then the operation should fail with "not a full content hash"
    And the queue should be empty

  Scenario: Play from empty queue
    When I play from queue
    Then the operation should fail with "Queue is empty"

  Scenario: Clear the queue
    Given the queue contains "aabb"
    And the queue contains "ccdd"
    When I clear the queue
    Then the queue should be empty

  Scenario: Prepend to queue
    Given the queue contains "aabb"
    When I prepend "ccdd" to the queue
    And I play from queue
    Then the now playing track should be "ccdd"
