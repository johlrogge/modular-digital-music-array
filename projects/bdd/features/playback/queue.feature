Feature: Queue management

  Scenario: Append and play from queue
    Given the queue contains "sha256:aabb"
    When I append "sha256:ccdd" to the queue
    And I play from queue
    Then the now playing track should be "sha256:aabb"
    And the queue should contain 1 track

  Scenario: Play from empty queue
    When I play from queue
    Then the operation should fail with "Queue is empty"

  Scenario: Clear the queue
    Given the queue contains "sha256:aabb"
    And the queue contains "sha256:ccdd"
    When I clear the queue
    Then the queue should be empty

  Scenario: Prepend to queue
    Given the queue contains "sha256:aabb"
    When I prepend "sha256:ccdd" to the queue
    And I play from queue
    Then the now playing track should be "sha256:ccdd"
