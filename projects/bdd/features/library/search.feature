Feature: Library search

  Background:
    Given the library contains:
      | artist                 | title         | bpm | genre     |
      | Carbon Based Lifeforms | Interloper    | 120 | Ambient   |
      | Carbon Based Lifeforms | MOS 6581      | 130 | Downtempo |
      | Extrawelt              | Soopertrack   | 132 | Techno    |

  Scenario: Search by artist
    When I search with artist "Carbon Based"
    Then the results should be:
      | artist                 | title      | bpm |
      | Carbon Based Lifeforms | Interloper | 120 |
      | Carbon Based Lifeforms | MOS 6581   | 130 |

  Scenario: Search by BPM tolerance
    When I search with bpm 130 tolerance 2
    Then the results should be:
      | artist                 | title       | bpm |
      | Carbon Based Lifeforms | MOS 6581    | 130 |
      | Extrawelt              | Soopertrack | 132 |

  Scenario: Combined search
    When I search with artist "Carbon Based" and bpm 130
    Then the results should be:
      | artist                 | title    | bpm |
      | Carbon Based Lifeforms | MOS 6581 | 130 |

  Scenario: Search by genre
    When I search with genre "Techno"
    Then the results should be:
      | artist    | title       | bpm |
      | Extrawelt | Soopertrack | 132 |

  Scenario: List all tracks
    When I list all tracks
    Then the results should be:
      | artist                 | title       | bpm |
      | Carbon Based Lifeforms | Interloper  | 120 |
      | Carbon Based Lifeforms | MOS 6581    | 130 |
      | Extrawelt              | Soopertrack | 132 |

  Scenario: Ambiguous hash resolution with legacy short-hash entity does not panic
    Given the library also contains a legacy track with raw hash "7926386d" and title "LegacyTrack" by "OldArtist"
    And the library also contains a legacy track with raw hash "79263890" and title "OtherLegacy" by "OldArtist"
    When I resolve hash "792638"
    Then the operation should fail with "Ambiguous"
